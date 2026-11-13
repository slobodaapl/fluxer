use collections::HashMap;

#[derive(Debug, Hash, PartialEq, Eq)]
pub(crate) enum SerialKind {
    DataDevice,
    InputMethod,
    MouseEnter,
    MousePress,
    KeyPress,
}

#[derive(Debug)]
struct SerialData {
    serial: u32,
}

impl SerialData {
    fn new(value: u32) -> Self {
        Self { serial: value }
    }
}

#[derive(Debug)]
/// Helper for tracking of different serial kinds.
pub(crate) struct SerialTracker {
    serials: HashMap<SerialKind, SerialData>,
    latest_input: u32,
}

impl SerialTracker {
    pub fn new() -> Self {
        Self {
            serials: HashMap::default(),
            latest_input: 0,
        }
    }

    pub fn update(&mut self, kind: SerialKind, value: u32) {
        if matches!(kind, SerialKind::MousePress | SerialKind::KeyPress) {
            self.latest_input = value;
        }
        self.serials.insert(kind, SerialData::new(value));
    }

    /// Latest keyboard or pointer input by arrival order, including serial wraparound.
    pub fn latest_input(&self) -> u32 {
        self.latest_input
    }

    /// Returns the latest tracked serial of the provided [`SerialKind`]
    ///
    /// Will return 0 if not tracked.
    pub fn get(&self, kind: SerialKind) -> u32 {
        self.serials
            .get(&kind)
            .map(|serial_data| serial_data.serial)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_input_tracks_arrival_and_ignores_unrelated_serials() {
        let mut tracker = SerialTracker::new();
        assert_eq!(tracker.latest_input(), 0);
        tracker.update(SerialKind::KeyPress, 5);
        assert_eq!(tracker.latest_input(), 5);
        tracker.update(SerialKind::MousePress, u32::MAX);
        assert_eq!(tracker.latest_input(), u32::MAX);
        tracker.update(SerialKind::MouseEnter, 100);
        assert_eq!(tracker.latest_input(), u32::MAX);
        tracker.update(SerialKind::KeyPress, 0);
        assert_eq!(tracker.latest_input(), 0);
        tracker.update(SerialKind::MousePress, 1);
        assert_eq!(tracker.latest_input(), 1);
        assert_eq!(tracker.get(SerialKind::KeyPress), 0);
    }
}
