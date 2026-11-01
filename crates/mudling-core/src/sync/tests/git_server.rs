//! A git server over HTTP on a local socket, in front of a bare repo: enough
//! of the smart protocol for a real fetch and push, and able to fail as a real
//! server does. It answers only this machine.

use git2::{Buf, Oid, Repository};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const MAIN: &str = "refs/heads/main";
const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

type Hook = Box<dyn FnOnce() + Send>;

struct Shared {
    bare: PathBuf,
    failing: AtomicBool,
    sign_in: AtomicBool,
    raced: AtomicBool,
    before_push: Mutex<Option<Hook>>,
}

pub(super) struct GitServer {
    port: u16,
    shared: Arc<Shared>,
}

impl GitServer {
    pub(super) fn start(bare: &Path) -> GitServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared {
            bare: bare.to_owned(),
            failing: AtomicBool::new(false),
            sign_in: AtomicBool::new(false),
            raced: AtomicBool::new(false),
            before_push: Mutex::new(None),
        });
        let serving = Arc::clone(&shared);
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                serve(client, &serving);
            }
        });
        GitServer { port, shared }
    }

    pub(super) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/r.git", self.port)
    }

    /// Answers everything with a 503 until told otherwise.
    pub(super) fn fail_with_503(&self, failing: bool) {
        self.shared.failing.store(failing, Ordering::Relaxed);
    }

    /// Answers everything with a demand to sign in.
    pub(super) fn ask_for_sign_in(&self) {
        self.shared.sign_in.store(true, Ordering::Relaxed);
    }

    /// Runs `hook` after a push has read the branch and before the server
    /// takes it, which is how a push loses a race.
    pub(super) fn before_next_push(&self, hook: impl FnOnce() + Send + 'static) {
        *self.shared.before_push.lock().unwrap() = Some(Box::new(hook));
    }

    pub(super) fn lost_a_race(&self) -> bool {
        self.shared.raced.load(Ordering::Relaxed)
    }
}

/// A request: its first line and its body.
fn read_request(client: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut reader = BufReader::new(client);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut length = 0;
    let mut chunked = false;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().ok()?;
        }
        chunked |= header == "transfer-encoding: chunked";
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).ok()?;
            chunk.truncate(size);
            body.append(&mut chunk);
            if size == 0 {
                break;
            }
        }
    } else {
        body.resize(length, 0);
        reader.read_exact(&mut body).ok()?;
    }
    Some((first, body))
}

fn serve(mut client: TcpStream, shared: &Shared) {
    let Some((request, body)) = read_request(&mut client) else {
        return;
    };
    if shared.sign_in.load(Ordering::Relaxed) {
        let refusal = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\n\
                       Content-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = client.write_all(refusal.as_bytes());
        return;
    }
    if shared.failing.load(Ordering::Relaxed) {
        let outage = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\
                      Connection: close\r\n\r\n";
        let _ = client.write_all(outage.as_bytes());
        return;
    }
    let repo = Repository::open_bare(&shared.bare).unwrap();
    let (kind, answer) = if request.contains("service=git-upload-pack") {
        (
            "git-upload-pack-advertisement",
            advertise(&repo, "upload-pack"),
        )
    } else if request.contains("service=git-receive-pack") {
        (
            "git-receive-pack-advertisement",
            advertise(&repo, "receive-pack"),
        )
    } else if request.contains("git-upload-pack") {
        ("git-upload-pack-result", upload(&repo, &body))
    } else {
        ("git-receive-pack-result", receive(&repo, &body, shared))
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-{kind}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        answer.len()
    );
    let _ = client.write_all(head.as_bytes());
    let _ = client.write_all(&answer);
}

fn packet(text: &str) -> Vec<u8> {
    let mut line = format!("{:04x}", text.len() + 4).into_bytes();
    line.extend_from_slice(text.as_bytes());
    line
}

fn tip(repo: &Repository) -> Option<Oid> {
    repo.refname_to_id(MAIN).ok()
}

fn advertise(repo: &Repository, service: &str) -> Vec<u8> {
    let mut answer = packet(&format!("# service=git-{service}\n"));
    answer.extend_from_slice(b"0000");
    let capabilities = if service == "receive-pack" {
        "report-status"
    } else {
        "no-progress"
    };
    answer.extend(packet(&match tip(repo) {
        Some(tip) => format!("{tip} {MAIN}\0{capabilities}\n"),
        None => format!("{NO_COMMIT} capabilities^{{}}\0{capabilities}\n"),
    }));
    answer.extend_from_slice(b"0000");
    answer
}

/// Answers once the client says it's done, with a pack of all it wanted.
fn upload(repo: &Repository, body: &[u8]) -> Vec<u8> {
    let mut answer = packet("NAK\n");
    if !body.windows(4).any(|window| window == b"done") {
        return answer;
    }
    let text = String::from_utf8_lossy(body);
    let mut walk = repo.revwalk().unwrap();
    for want in text.match_indices("want ") {
        let id = &text[want.0 + 5..want.0 + 45];
        walk.push(Oid::from_str(id).unwrap()).unwrap();
    }
    let mut pack = repo.packbuilder().unwrap();
    pack.insert_walk(&mut walk).unwrap();
    let mut buffer = Buf::new();
    pack.write_buf(&mut buffer).unwrap();
    answer.extend_from_slice(&buffer);
    answer
}

fn receive(repo: &Repository, body: &[u8], shared: &Shared) -> Vec<u8> {
    let length = usize::from_str_radix(std::str::from_utf8(&body[..4]).unwrap(), 16).unwrap();
    let command = String::from_utf8_lossy(&body[4..length]).into_owned();
    let mut words = command.split([' ', '\0']);
    let (old, new) = (words.next().unwrap(), words.next().unwrap());
    let pack = &body[length + 4..];

    if let Some(hook) = shared.before_push.lock().unwrap().take() {
        hook();
    }
    let at = tip(repo).map_or(NO_COMMIT.to_owned(), |tip| tip.to_string());
    let mut answer = packet("unpack ok\n");
    if at == old {
        let odb = repo.odb().unwrap();
        let mut writer = odb.packwriter().unwrap();
        writer.write_all(pack).unwrap();
        writer.commit().unwrap();
        repo.reference(MAIN, Oid::from_str(new).unwrap(), true, "push")
            .unwrap();
        answer.extend(packet(&format!("ok {MAIN}\n")));
    } else {
        shared.raced.store(true, Ordering::Relaxed);
        answer.extend(packet(&format!(
            "ng {MAIN} cannot lock ref '{MAIN}': is at {at} but expected {old}\n"
        )));
    }
    answer.extend_from_slice(b"0000");
    answer
}
