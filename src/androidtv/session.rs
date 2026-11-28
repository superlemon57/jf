//! Remote session (port 6466), open to paired phones only. Like a real device, jf
//! describes itself first, activates the session, says it is on, then pings the phone
//! every 5 seconds; the phone sends Android key codes.

use std::io;
use std::time::{Duration, Instant};

use super::proto::{field, fields, Inbox, Message, Value};
use super::{finish, handshake, receive, send, trace, Shared, Tls};
use crate::remote::{self, Command};

/// Keys, power and volume (the phone's volume buttons), app links: no keyboard or voice,
/// so the phone does not offer them. A real device sends 622 (with both).
const FEATURES: u64 = 0x262;
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// Silence after which the phone is gone (three unanswered pings, as on a real device).
const SILENCE: Duration = Duration::from_secs(16);
/// A held key repeats after this delay, then at this interval.
const REPEAT_DELAY: Duration = Duration::from_millis(450);
const REPEAT_INTERVAL: Duration = Duration::from_millis(130);
const SEEK_STEP: f64 = 10.0;

// Fields of the remote message.
const CONFIGURE: u32 = 1;
const SET_ACTIVE: u32 = 2;
const ERROR: u32 = 3;
const PING_REQUEST: u32 = 8;
const PING_RESPONSE: u32 = 9;
const KEY_INJECT: u32 = 10;
const START: u32 = 40;
const SET_VOLUME_LEVEL: u32 = 50;
const ADJUST_VOLUME_LEVEL: u32 = 51;

// Key directions.
const START_LONG: u64 = 1;
const END_LONG: u64 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Key(&'static str),
    Seek(f64),
    Volume(f64),
    Mute,
}

impl Action {
    /// Android key code (`android.view.KeyEvent`).
    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            19 => Action::Key("up"),
            20 => Action::Key("down"),
            21 => Action::Key("left"),
            22 => Action::Key("right"),
            23 | 66 | 160 => Action::Key("ok"),
            4 | 111 => Action::Key("back"),
            3 => Action::Key("home"),
            82 => Action::Key("menu"),
            85 => Action::Key("playpause"),
            126 => Action::Key("play"),
            127 => Action::Key("pause"),
            86 => Action::Key("stop"),
            87 => Action::Key("next"),
            88 => Action::Key("previous"),
            89 => Action::Seek(-SEEK_STEP),
            90 => Action::Seek(SEEK_STEP),
            24 => Action::Volume(1.0 / 16.0),
            25 => Action::Volume(-1.0 / 16.0),
            91 | 164 => Action::Mute,
            _ => return None,
        })
    }

    fn repeats(self) -> bool {
        matches!(self, Action::Key("up" | "down" | "left" | "right") | Action::Seek(_) | Action::Volume(_))
    }
}

struct Held {
    action: Action,
    next: Instant,
}

pub(super) fn serve(mut tls: Tls, shared: &Shared, id: u64) -> io::Result<()> {
    tls.sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    tls.sock.set_nodelay(true)?;
    handshake(&mut tls, Duration::from_secs(10))?;
    let result = Session { tls: &mut tls, shared, id, held: None }.run();
    finish(&mut tls);
    result
}

struct Session<'a> {
    tls: &'a mut Tls,
    shared: &'a Shared,
    id: u64,
    held: Option<Held>,
}

impl Session<'_> {
    fn run(&mut self) -> io::Result<()> {
        let device = Message::new()
            .text(1, "jf")
            .text(2, "jf")
            .uint(3, 1)
            .text(4, "1")
            .text(5, "com.google.android.tv.remote.service")
            .text(6, env!("CARGO_PKG_VERSION"));
        self.send(Message::new().message(CONFIGURE, Message::new().uint(1, FEATURES).message(2, device)))?;
        let mut inbox = Inbox::default();
        let (mut heard, mut pinged, mut pings) = (Instant::now(), Instant::now(), 0u64);
        loop {
            if let Some(message) = receive(self.tls, &mut inbox)? {
                heard = Instant::now();
                self.handle(&message)?;
                continue;
            }
            if heard.elapsed() > SILENCE {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "phone silent"));
            }
            if pinged.elapsed() >= PING_INTERVAL {
                pinged = Instant::now();
                pings += 1;
                self.send(Message::new().message(PING_REQUEST, Message::new().uint(1, pings).uint(2, pings)))?;
            }
            self.repeat()?;
        }
    }

    fn send(&mut self, message: Message) -> io::Result<()> {
        send(self.tls, &message)
    }

    fn handle(&mut self, message: &[u8]) -> io::Result<()> {
        let Some((number, value)) = fields(message).next() else { return Ok(()) };
        let content = value.bytes().unwrap_or_default();
        let id = self.id;
        match number {
            CONFIGURE => {
                let device = field(content, 2).and_then(Value::bytes).unwrap_or_default();
                let text = |number| field(device, number).map(Value::text).unwrap_or_default();
                trace!("#{id}: phone {} {} ({} {})", text(2), text(1), text(5), text(6));
                self.send(Message::new().message(SET_ACTIVE, Message::new()))?;
            }
            SET_ACTIVE => {
                let active = field(content, 1).and_then(Value::int).unwrap_or(0);
                trace!("#{id}: remote active (features {active:#x})");
                self.send(Message::new().message(START, Message::new().uint(1, 1)))?;
                self.send_volume()?;
            }
            PING_REQUEST => {
                let value = field(content, 1).and_then(Value::int).unwrap_or(0);
                self.send(Message::new().message(PING_RESPONSE, Message::new().uint(1, value)))?;
            }
            PING_RESPONSE => {}
            KEY_INJECT => {
                let code = field(content, 1).and_then(Value::int).unwrap_or(0);
                let direction = field(content, 2).and_then(Value::int).unwrap_or(0);
                self.key(code, direction)?;
            }
            SET_VOLUME_LEVEL => {
                let level = field(content, 7).and_then(Value::int);
                let max = field(content, 6).and_then(Value::int).filter(|max| *max > 0).unwrap_or(100);
                trace!("#{id}: volume set to {level:?}/{max}");
                if let Some(level) = level {
                    remote::set_volume(level.min(max) as f64 / max as f64);
                }
                self.send_volume()?;
            }
            ADJUST_VOLUME_LEVEL => trace!("#{id}: volume adjust {}", super::hex(content)),
            ERROR => trace!("#{id}: error reported by the phone"),
            other => trace!("#{id}: message {other} ignored ({})", super::hex(content)),
        }
        Ok(())
    }

    fn key(&mut self, code: u64, direction: u64) -> io::Result<()> {
        let action = Action::from_code(code);
        trace!("#{}: key {code} (direction {direction})", self.id);
        if direction == END_LONG {
            self.held = None;
            return Ok(());
        }
        let Some(action) = action else { return Ok(()) };
        self.held =
            (direction == START_LONG && action.repeats()).then(|| Held { action, next: Instant::now() + REPEAT_DELAY });
        self.perform(action)
    }

    fn repeat(&mut self) -> io::Result<()> {
        let Some(held) = self.held.as_mut().filter(|held| Instant::now() >= held.next) else { return Ok(()) };
        held.next = Instant::now() + REPEAT_INTERVAL;
        let action = held.action;
        self.perform(action)
    }

    fn perform(&mut self, action: Action) -> io::Result<()> {
        match action {
            Action::Key(key) => self.shared.send(Command::Key(key.to_owned())),
            Action::Seek(offset) => self.shared.send(Command::SeekBy(offset)),
            Action::Volume(delta) => {
                remote::change_volume(delta);
                self.send_volume()?;
            }
            Action::Mute => {
                remote::toggle_mute();
                self.send_volume()?;
            }
        }
        Ok(())
    }

    /// System volume, shown by the phone; nothing where jf cannot read it.
    fn send_volume(&mut self) -> io::Result<()> {
        let Some(volume) = remote::volume() else { return Ok(()) };
        let level = (volume.clamp(0.0, 1.0) * 100.0).round() as u64;
        let muted = u64::from(remote::muted());
        // Fields 1, 2 and 4 are not understood, but every real device sends them (field 2 is
        // always 9) and the Google TV app ignores its volume buttons without them.
        let content =
            Message::new().uint(1, 1).uint(2, 9).text(3, "jf").uint(4, 1).uint(6, 100).uint(7, level).uint(8, muted);
        self.send(Message::new().message(SET_VOLUME_LEVEL, content))
    }
}
