//! The map: rooms built live from GMCP `Room.Info`, the game's text and the
//! player's commands (PRD §7), one per server and shared by its characters.
//!
//! A move costs a few room updates in memory and a batch of the rows that
//! changed; nothing re-encodes the world. Rooms are told apart by the GMCP
//! room number when there is one, otherwise by name, exits and description,
//! keeping several explanations while unsure (see `tracker`). Areas that
//! stop lining up are redrawn (`straighten`), lifting buildings out when
//! that draws them better (`buildings`).
//!
//! The game's wording comes from [`MapRules`]; the core holds none.
//!
//! ```
//! use mudling_core::map::{LiveMap, PlainRules};
//! use mudling_core::persist::Timestamp;
//!
//! let mut map = LiveMap::new(Box::new(PlainRules));
//! let now = Timestamp(0);
//! map.room_info(r#"{"num": 1, "name": "A road", "zone": "Fields", "exits": {"n": 2}}"#, now);
//! map.sent("north", now);
//! map.room_info(r#"{"num": 2, "name": "A gate", "zone": "Fields", "exits": {"s": 1}}"#, now);
//! let commit = map.commit(now).expect("two rooms were mapped");
//! assert_eq!(commit.change.rooms.len(), 2);
//! // `commit.batch` goes to the persistence writer; `commit.change` to views.
//! ```

mod buildings;
mod geometry;
mod info;
mod layout;
mod live;
mod mobs;
mod room;
mod route;
mod rows;
mod rules;
mod straighten;
mod text;
mod tracker;

pub use buildings::MapDoor;
pub use geometry::{DirMap, DirSet, Direction, GridPoint};
pub use info::{ExitInfo, RoomInfo};
pub use live::{Area, Building, Commit, Derived, LiveMap, MapChange, Whereabouts};
pub use mobs::{Mark, MarkChange, MobMemory, mob_name};
pub use room::{ExitState, MapRoom, RoomId, feature};
pub use route::WalkStep;
pub use rules::{AreaName, MapRules, PlainRules, RoomIdentity};
pub use tracker::Arrived;

#[cfg(test)]
mod tests;
