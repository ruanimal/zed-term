use std::{io, sync::Arc};

use alacritty_terminal::{
    event::{OnResize, WindowSize},
    tty::{ChildEvent, EventedPty, EventedReadWrite},
};
use futures::channel::mpsc::UnboundedSender;
use polling::{Event, PollMode, Poller};

use crate::{PtyEvent, TerminalBackendEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    Normal,
    Error,
    Indeterminate,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalProgress {
    pub state: ProgressState,
    pub percentage: Option<u8>,
}

#[derive(Default)]
pub(crate) struct ProgressParser {
    parser: vte::Parser,
    handler: ProgressHandler,
}

impl ProgressParser {
    pub(crate) fn advance(&mut self, mut bytes: &[u8]) -> Option<Option<TerminalProgress>> {
        while !bytes.is_empty() {
            if let Some(progress) = self.handler.pending.take() {
                // vte dispatches OSC on ESC, before confirming the backslash.
                // Only the immediately following byte may complete this OSC.
                if bytes.first() == Some(&b'\\') {
                    self.handler.set(progress);
                }
                self.parser.advance(&mut self.handler, &bytes[..1]);
                bytes = &bytes[1..];
            } else {
                let consumed = self
                    .parser
                    .advance_until_terminated(&mut self.handler, bytes);
                bytes = &bytes[consumed..];
            }
        }
        self.handler.changed.take()
    }
}

#[derive(Default)]
struct ProgressHandler {
    progress: Option<TerminalProgress>,
    changed: Option<Option<TerminalProgress>>,
    pending: Option<Option<TerminalProgress>>,
}

impl ProgressHandler {
    fn set(&mut self, progress: Option<TerminalProgress>) {
        if self.progress != progress {
            self.progress = progress;
            self.changed = Some(progress);
        }
    }
}

impl vte::Perform for ProgressHandler {
    fn osc_dispatch(&mut self, parameters: &[&[u8]], bell_terminated: bool) {
        self.pending = None;
        let [b"9", b"4", remaining @ ..] = parameters else {
            return;
        };
        let (state, percentage) = match remaining {
            [] => (b"0".as_slice(), None),
            [state] => (*state, None),
            // Cargo leaves the optional percentage empty when clearing progress.
            [state, b""] if matches!(*state, b"0" | b"2" | b"3" | b"4") => (*state, None),
            [state, percentage] => {
                let Some(percentage) = parse_percentage(percentage) else {
                    return;
                };
                (*state, Some(percentage))
            }
            _ => return,
        };
        let previous_percentage = self.progress.and_then(|progress| progress.percentage);
        let progress = match state {
            b"0" => None,
            b"1" => Some(TerminalProgress {
                state: ProgressState::Normal,
                percentage: Some(percentage.unwrap_or(0)),
            }),
            b"2" => Some(TerminalProgress {
                state: ProgressState::Error,
                percentage: percentage.or(previous_percentage),
            }),
            b"3" => Some(TerminalProgress {
                state: ProgressState::Indeterminate,
                percentage: previous_percentage,
            }),
            b"4" => Some(TerminalProgress {
                state: ProgressState::Paused,
                percentage: percentage.or(previous_percentage),
            }),
            _ => return,
        };
        if bell_terminated {
            self.set(progress);
        } else {
            self.pending = Some(progress);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && intermediates.is_empty() && byte == b'c' {
            self.set(None);
        }
    }

    fn execute(&mut self, _byte: u8) {
        // CAN/SUB also dispatch an unfinished OSC, then execute the cancel.
        self.pending = None;
    }

    fn terminated(&self) -> bool {
        self.pending.is_some()
    }
}

fn parse_percentage(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u8, |value, byte| {
        byte.is_ascii_digit().then(|| {
            value
                .saturating_mul(10)
                .saturating_add(byte - b'0')
                .min(100)
        })
    })
}

pub(crate) struct ProgressPty<P> {
    inner: P,
    parser: ProgressParser,
    events: UnboundedSender<PtyEvent>,
}

impl<P> ProgressPty<P> {
    pub(crate) fn new(inner: P, events: UnboundedSender<PtyEvent>) -> Self {
        Self {
            inner,
            parser: ProgressParser::default(),
            events,
        }
    }
}

impl<P: EventedPty> io::Read for ProgressPty<P> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.reader().read(buffer)?;
        if let Some(progress) = self.parser.advance(&buffer[..count]) {
            if let Err(error) =
                self.events
                    .unbounded_send(PtyEvent::Event(TerminalBackendEvent::ProgressChanged(
                        progress,
                    )))
            {
                log::debug!("terminal no longer receives progress events: {error}");
            }
        }
        Ok(count)
    }
}

impl<P: EventedPty> EventedReadWrite for ProgressPty<P> {
    type Reader = Self;
    type Writer = P::Writer;

    unsafe fn register(
        &mut self,
        poller: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // The wrapper retains the PTY for the lifetime of its registration.
        unsafe { self.inner.register(poller, interest, mode) }
    }

    fn reregister(
        &mut self,
        poller: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.inner.reregister(poller, interest, mode)
    }

    fn deregister(&mut self, poller: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poller)
    }

    fn reader(&mut self) -> &mut Self::Reader {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl<P: EventedPty> EventedPty for ProgressPty<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl<P: OnResize> OnResize for ProgressPty<P> {
    fn on_resize(&mut self, size: WindowSize) {
        self.inner.on_resize(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_survives_every_read_boundary() {
        for sequence in [b"\x1b]9;4;1;42\x07".as_slice(), b"\x1b]9;4;1;42\x1b\\"] {
            for boundary in 0..sequence.len() {
                let mut parser = ProgressParser::default();
                assert_eq!(parser.advance(&sequence[..boundary]), None);
                assert_eq!(
                    parser.advance(&sequence[boundary..]),
                    Some(Some(TerminalProgress {
                        state: ProgressState::Normal,
                        percentage: Some(42),
                    }))
                );
            }
        }
    }

    #[test]
    fn cargo_style_clear_survives_every_read_boundary() {
        for sequence in [b"\x1b]9;4;0;\x07".as_slice(), b"\x1b]9;4;0;\x1b\\"] {
            for boundary in 0..sequence.len() {
                let mut parser = ProgressParser::default();
                parser.advance(b"\x1b]9;4;1;50\x07");
                assert_eq!(parser.advance(&sequence[..boundary]), None);
                assert_eq!(parser.advance(&sequence[boundary..]), Some(None));
            }
        }
    }

    #[test]
    fn states_preserve_optional_values_and_clear_explicitly() {
        let mut parser = ProgressParser::default();
        for (sequence, expected) in [
            ("1;50", Some((ProgressState::Normal, Some(50)))),
            ("2;", Some((ProgressState::Error, Some(50)))),
            ("4;", Some((ProgressState::Paused, Some(50)))),
            ("3;", Some((ProgressState::Indeterminate, Some(50)))),
            ("1;100", Some((ProgressState::Normal, Some(100)))),
            ("0;", None),
            ("2", Some((ProgressState::Error, None))),
            ("4;25", Some((ProgressState::Paused, Some(25)))),
            (
                "1;999999999999999999999",
                Some((ProgressState::Normal, Some(100))),
            ),
            ("1", Some((ProgressState::Normal, Some(0)))),
        ] {
            assert_eq!(
                parser.advance(format!("\x1b]9;4;{sequence}\x07").as_bytes()),
                Some(expected.map(|(state, percentage)| TerminalProgress { state, percentage }))
            );
        }
        assert_eq!(parser.advance(b"\x1bc"), Some(None));
        parser.advance(b"\x1b]9;4;3\x07");
        assert_eq!(parser.advance(b"\x1b]9;4\x07"), Some(None));
    }

    #[test]
    fn malformed_or_unrelated_output_does_not_change_progress() {
        let mut parser = ProgressParser::default();
        parser.advance(b"\x1b]9;4;1;50\x07");
        for bytes in [
            b"50% 9;4;1;20".as_slice(),
            b"\x1b]9;hello\x07",
            b"\x1b]9;40;1;20\x07",
            b"\x1b]9;4;5;20\x07",
            b"\x1b]9;4;1;-1\x07",
            b"\x1b]9;4;1;50.5\x07",
            b"\x1b]9;4;1;\x07",
            b"\x1b]9;4;1;10;extra\x07",
            b"\x1b]9;4;0;\x18",
            b"\x1b]9;4;0;\x1a",
            b"\x1b]9;4;0;\x1bXignored\x1b\\",
            b"\x1b]9;4;1;20\x18",
            b"\x1b]9;4;1;20\x1a",
            b"\x1b]9;4;1;20\x1bXignored\x1b\\",
            b"\x1b]9;4;1;20\x1bP\x1b\\",
            b"\x1b]9;4;1;20\x1b\x1b\\",
            b"\x1bPignored 9;4;1;20\x1b\\",
            b"\x1b]9;4;1;50\x07",
        ] {
            assert_eq!(parser.advance(bytes), None, "{bytes:?}");
        }
        assert_eq!(parser.advance(b"\x1b]9;4;0\x07"), Some(None));
    }
}
