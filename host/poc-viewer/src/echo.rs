//! Settings the window sent and the board has not shown yet, so that a state answering an
//! older request does not overwrite a newer edit, while a change the board makes on its own
//! (the threshold scaled by a mode change to the other format) still reaches the editor.

/// Most settings kept in flight; older ones are forgotten.
const IN_FLIGHT: usize = 32;

/// The settings in flight, oldest first, and the board's settings last seen.
#[derive(Debug, Clone)]
pub struct Echoes<T> {
    sent: Vec<T>,
    board: Option<T>,
}

impl<T> Default for Echoes<T> {
    fn default() -> Self {
        Self {
            sent: Vec::new(),
            board: None,
        }
    }
}

impl<T: Copy + PartialEq> Echoes<T> {
    /// `settings` were sent. With nothing in flight, settings the board already has change
    /// nothing it shows, so they are not waited for.
    pub fn sent(&mut self, settings: T) {
        if self.sent.is_empty() && self.board == Some(settings) {
            return;
        }
        if self.sent.len() == IN_FLIGHT {
            self.sent.remove(0);
        }
        self.sent.push(settings);
    }

    /// The board's settings in a state. Returns the settings the editor takes: `None` when
    /// they did not change or they are the echo of a request (that request and every older
    /// one are done); the board's when it changed them itself (nothing is in flight then).
    pub fn board(&mut self, settings: T) -> Option<T> {
        if self.board == Some(settings) {
            return None;
        }
        self.board = Some(settings);
        if let Some(i) = self.sent.iter().position(|s| *s == settings) {
            self.sent.drain(..=i);
            return None;
        }
        self.sent.clear();
        Some(settings)
    }

    /// The editor took the board's settings as they are: nothing is in flight.
    pub fn adopted(&mut self, settings: T) {
        self.sent.clear();
        self.board = Some(settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_echo_is_ignored_and_a_board_change_is_taken() {
        let mut e = Echoes::default();
        e.adopted(128);
        e.sent(0);
        e.sent(128);
        // The echoes of both requests.
        assert_eq!(e.board(0), None);
        assert_eq!(e.board(128), None);
        // A mode change to RAW10 scales the threshold, and back to RAW8 again: the second is
        // equal to a request answered long ago, and still a change of the board's own.
        assert_eq!(e.board(512), Some(512));
        assert_eq!(e.board(128), Some(128));
        // The same state again changes nothing.
        assert_eq!(e.board(128), None);
    }

    #[test]
    fn an_older_echo_keeps_the_newer_edit() {
        let mut e = Echoes::default();
        e.adopted(10);
        e.sent(20);
        e.sent(30);
        // The answer to the first request: the second is still in flight.
        assert_eq!(e.board(20), None);
        assert_eq!(e.board(30), None);
        // With nothing in flight, settings the board already has are not waited for: a
        // later change of the board's own back to them is taken.
        e.sent(30);
        assert_eq!(e.board(40), Some(40));
        assert_eq!(e.board(30), Some(30));
    }
}
