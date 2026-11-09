//! Providerek: mindegyik egy eszközcsaládot kezel, és közös `DeviceState`-et ad vissza.

pub mod hid;
pub mod wired_detect;

use crate::model::DeviceState;

pub trait Provider {
    /// A provider által ismert összes eszköz jelenlegi állapota. Az eszközszál hívja induláskor
    /// és minden eszközváltozás (csatlakozás, lecsatlakozás) után; csak a változások mennek a UI-nak.
    fn scan(&mut self) -> Vec<DeviceState>;
}
