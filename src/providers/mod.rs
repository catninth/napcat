//! Providerek: mindegyik egy eszközcsaládot kezel, és közös `DeviceState`-et ad vissza.

pub mod bt_pnp;
pub mod compx;
pub mod custom;
pub mod hid;
pub mod logitech_hidpp;
pub mod mchose;
pub mod playstation;
pub mod wired_detect;
pub mod xinput;

use std::time::Duration;

use crate::model::DeviceState;

pub trait Provider {
    /// A provider által ismert összes eszköz jelenlegi állapota. Az eszközszál hívja induláskor,
    /// minden eszközváltozás (csatlakozás, lecsatlakozás) után, és ha van, `poll_interval`
    /// időközönként; csak a változások mennek a UI-nak.
    fn scan(&mut self) -> Vec<DeviceState>;

    /// Időzített újraolvasás, ha az eszköz nem jelez magától. None: tisztán eseményvezérelt
    /// (vagy épp nincs csatlakoztatott eszköze, akkor nincs mit olvasni).
    fn poll_interval(&self) -> Option<Duration> {
        None
    }
}
