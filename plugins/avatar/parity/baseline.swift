// The parity baseline: what the prototype's own code reads from its session
// logs. `avatar-parity` copies this into a copy of mud-client-spike and runs
// it there; the spike itself is never built or changed.
//
// It drives MUDKit in the order AppModel.handle(_:) does, without the app:
// each log is one connection, on a world saved and loaded between them. The
// logs don't hold what was typed, beyond moves and looks, so a typed identify
// can't name its copy, and nothing here sends commands.
//
//     parity-baseline <out.json> <session.log>…

import Foundation
import MUDKit

/// One thing a log recorded, in order.
enum Happened {
    case text(String)
    case gmcp(GMCPMessage)
    case map(MapNote)
}

/// The prototype's notes in a log. SessionLogger wraps each in newlines of
/// its own, so what's between two notes is the server's text exactly, and
/// a prompt it left unended stays unended.
func events(of log: String) -> [Happened] {
    var happened: [Happened] = []
    var text: [String] = []
    func endText() {
        let joined = text.joined(separator: "\n")
        if !joined.isEmpty { happened.append(.text(joined)) }
        text = []
    }
    // Lines are cut as bytes: "\r\n" is one Character.
    for bytes in log.utf8.split(separator: UInt8(ascii: "\n"), omittingEmptySubsequences: false) {
        let line = String(decoding: bytes, as: UTF8.self)
        if line.hasPrefix("[GMCP] ") {
            endText()
            let message = line.dropFirst("[GMCP] ".count)
            let package = message.prefix { $0 != " " }
            let json = message.dropFirst(package.count).trimmingCharacters(in: .whitespacesAndNewlines)
            happened.append(.gmcp(GMCPMessage(package: String(package), json: json.isEmpty ? nil : json)))
        } else if line.hasPrefix("[MAP] ") {
            endText()
            if let note = MapNote(logLine: line) { happened.append(.map(note)) }
        } else if line == "[connected]" || line.hasPrefix("[telnet] ") || line.hasPrefix("[auto-login] ")
            || line.hasPrefix("[disconnected] ") {
            endText()
        } else {
            text.append(line)
        }
    }
    endText()
    return happened
}

/// What MUDPad saves, as JSON.
struct Saved: Encodable {
    var map: AutoMap
    var mobs: MobMemory
    var characters: [String: CharacterRecord]
    var gear: GearDatabase
}

/// A session's work from AppModel, on a clock that stands still between rooms.
@MainActor
final class Replay {
    var map = AutoMap()
    var mobs = MobMemory()
    var characters: [String: CharacterRecord] = [:]
    var gear = GearDatabase()

    private var clock = Date(timeIntervalSince1970: 0)
    private var feed = MapFeed()
    private var game = GameState()
    private var prompt = GamePrompt()
    private var splitter = LineSplitter()
    private var parser = CharacterParser()
    private var identifies = IdentifyReader()
    private var examines = ExamineReader()
    private var gearChange: GearChange?
    private var inGame = false
    private var nameAtLogin = ""
    private var welcomedName: String?

    private var activeName: String { game.characterName.isEmpty ? nameAtLogin : game.characterName }
    private var character: CharacterRecord { characters[activeName] ?? CharacterRecord() }
    private var vitalMaximums: [String: Int] {
        let vitals = game.vitals
        guard vitals.maxHP > 0 else { return [:] }
        return ["health_max": vitals.maxHP, "mana_max": vitals.maxMana, "movement_max": vitals.maxMoves]
    }

    private func stat(_ key: String) -> String? {
        vitalMaximums[key].map(String.init) ?? game.status[key] ?? character.stats[key]
    }

    func connection(_ log: String) throws {
        // The world is saved and loaded between connections, which is what
        // ends any doubt the map held.
        map = try JSONDecoder().decode(AutoMap.self, from: JSONEncoder().encode(map))
        clock += 3600
        feed = MapFeed()
        game = GameState()
        prompt = GamePrompt()
        splitter = LineSplitter()
        parser = CharacterParser()
        identifies = IdentifyReader()
        examines = ExamineReader()
        gearChange = nil
        inGame = false
        nameAtLogin = ""
        welcomedName = nil
        for event in events(of: log) {
            switch event {
            case .text(let text): received(text)
            case .gmcp(let message): received(message)
            case .map(let note): feed.apply(note, to: &map, at: clock)
            }
        }
        finishGearChange()
    }

    /// The case .text of AppModel.handle(_:).
    private func received(_ text: String) {
        let hadPrompt = prompt.read(text: text)
        if hadPrompt { sharePrompt() }
        var changed = false
        for line in splitter.feed(text) { changed = read(line: line) || changed }
        feed.read(text: text, into: &map, at: clock)
        if !mobs.observe(text).isEmpty { map.noteFeature(.aggressiveMob) }
        if !inGame, let welcome = text.firstMatch(of: /Welcome back to the AVATAR System,?((?: \w+)+)/),
           let name = welcome.1.split(separator: " ").last {
            welcomedName = String(name)
        }
        if hadPrompt { gearChange?.notePrompt() }
        for event in parser.takeEvents() {
            switch event {
            case .wore(let item):
                let id = game.wore(item)
                if let id { characters[activeName]?.noteWornID(id, of: item) }
                beginGearChange()
                gearChange?.wore(GearItem(item, id: id))
            case .removed(let item, let noted):
                game.tookOff(item, id: noted)
                beginGearChange()
                gearChange?.removed(GearItem(item, id: noted))
            default:
                break
            }
        }
        if changed {
            for worn in character.equipment where worn.slot != "worn" {
                gear.learnSlot(worn.slot, of: worn.item)
            }
        }
    }

    /// AppModel.readCharacterLine(_:). Returns whether the record changed.
    private func read(line: String) -> Bool {
        feed.read(line: line)
        if let identified = identifies.read(line: line), let item = identifiedItem(identified) {
            gear.learn(identified, of: item, at: clock)
        }
        if let examined = examines.read(line: line) { gear.learnSlot(examined.slot, of: examined.item) }
        var record = character
        _ = parser.read(line: line, into: &record, at: clock)
        guard record != character, !activeName.isEmpty else { return false }
        characters[activeName] = record
        return true
    }

    /// AppModel.identifiedItem(_:), with no typed command to name a copy.
    private func identifiedItem(_ identified: ItemIdentification) -> GearItem? {
        let atHand = character.equipment.map(\.gearItem) + game.carriedGear + game.bagsOfGear.flatMap(\.items)
        guard let name = identified.name ?? ItemMatch.best(for: identified.keywords, among: atHand.map(\.name))
        else { return nil }
        let copies = atHand.filter { GearName.plain($0.name) == GearName.plain(name) }
        return copies.count == 1 ? copies[0] : GearItem(name)
    }

    /// The case .gmcp of AppModel.handle(_:), with updateWorld(after:).
    private func received(_ message: GMCPMessage) {
        game.apply(message)
        if message.package == "Char.Vitals" { prompt.vitalsArrived() }
        if message.package == "Room.Info" && !inGame {
            inGame = true
            nameAtLogin = welcomedName ?? ""
        }
        switch message.package {
        case "Room.Info":
            clock += 1
            feed.arrived(in: game.room, map: &map, at: clock)
        case "Room.WrongDir":
            map.moveFailed(at: clock)
        case "Char.Vitals":
            gearChange?.noteStats(vitalMaximums)
        case "Char.Status":
            if let values = message.decodedPayload as? [String: Any],
               gearChange?.noteStats(values.compactMapValues { ($0 as? String).flatMap(Int.init) }) == true {
                finishGearChange()
            }
            if message.json?.contains("room_exits") == true { map.refreshExits(game.room.exits) }
        case "Room.Players", "Room.AddPlayer":
            let seen = game.occupants.filter { Int($0.name) != nil }
            map.noteSightings(seen.map(\.title), at: clock)
            if seen.contains(where: mobs.isAggressive) { map.noteFeature(.aggressiveMob) }
        case "Char.Items.List", "Char.Items.Add":
            let carried = Set(game.inventory.map(\.id))
            if character.equipment.contains(where: { $0.id.map(carried.contains) == true }) {
                characters[activeName]?.forgetWornIDs(carried)
            }
            if game.roomItems.contains(where: { $0.type == "fountain" }) { map.noteFeature(.fountain) }
        default:
            break
        }
    }

    private func beginGearChange() {
        if let change = gearChange, change.sawPrompt { finishGearChange() }
        guard gearChange == nil else { return }
        let before = GearStat.keys.reduce(into: [String: Int]()) { stats, key in
            if let value = stat(key).flatMap(Int.init) { stats[key] = value }
        }
        gearChange = GearChange(before: before, at: clock)
    }

    private func finishGearChange() {
        guard let change = gearChange else { return }
        gearChange = nil
        if let measured = change.measured { gear.learnMeasured(measured.effects, of: measured.item, at: clock) }
    }

    private func sharePrompt() {
        parser.prompt = prompt
        feed.prompt = prompt
        identifies.prompt = prompt
    }

    /// AppModel.learnFromLogs(identifies:): where worn items go, from every
    /// log, for items nothing else placed.
    func learnSlots(fromLogs logs: [String]) {
        for log in logs {
            for bytes in log.utf8.split(separator: UInt8(ascii: "\n")) {
                let line = String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                guard line.hasPrefix("You "), let worn = CharacterParser.slotWornIn(line),
                      gear.slot(of: worn.item) == nil
                else { continue }
                gear.learnSlot(worn.slot, of: worn.item)
            }
        }
    }
}

@MainActor
func run(arguments: [String]) throws {
    guard arguments.count >= 3 else {
        FileHandle.standardError.write(Data("usage: parity-baseline <out.json> <session.log>…\n".utf8))
        exit(2)
    }
    let logs = try arguments.dropFirst(2).map { path in
        // As the app's readers: bytes that aren't UTF-8 are replaced.
        String(decoding: try Data(contentsOf: URL(fileURLWithPath: path)), as: UTF8.self)
    }
    let replay = Replay()
    for log in logs { try replay.connection(log) }
    replay.learnSlots(fromLogs: logs)
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .prettyPrinted]
    let saved = Saved(map: replay.map, mobs: replay.mobs, characters: replay.characters, gear: replay.gear)
    try encoder.encode(saved).write(to: URL(fileURLWithPath: arguments[1]))
}

try MainActor.assumeIsolated { try run(arguments: CommandLine.arguments) }
