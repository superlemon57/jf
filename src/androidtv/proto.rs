//! The little of Protocol Buffers the Android TV remote needs: messages built field by
//! field, read back as (number, value) pairs, framed by a varint length on the wire.

use std::io;

/// Largest message accepted from a phone (key presses and pairing steps are tiny).
const MAX_MESSAGE: u64 = 64 * 1024;

#[derive(Default)]
pub struct Message(Vec<u8>);

fn put_varint(buffer: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buffer.push(value as u8 | 0x80);
        value >>= 7;
    }
    buffer.push(value as u8);
}

impl Message {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn uint(mut self, field: u32, value: u64) -> Self {
        put_varint(&mut self.0, u64::from(field) << 3);
        put_varint(&mut self.0, value);
        self
    }

    pub fn bytes(mut self, field: u32, value: &[u8]) -> Self {
        put_varint(&mut self.0, u64::from(field) << 3 | 2);
        put_varint(&mut self.0, value.len() as u64);
        self.0.extend_from_slice(value);
        self
    }

    pub fn text(self, field: u32, value: &str) -> Self {
        self.bytes(field, value.as_bytes())
    }

    pub fn message(self, field: u32, value: Message) -> Self {
        self.bytes(field, &value.0)
    }

    /// The message preceded by its length, as written on the connection.
    pub fn delimited(&self) -> Vec<u8> {
        let mut framed = Vec::with_capacity(self.0.len() + 2);
        put_varint(&mut framed, self.0.len() as u64);
        framed.extend_from_slice(&self.0);
        framed
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

impl<'a> Value<'a> {
    pub fn int(self) -> Option<u64> {
        match self {
            Value::Int(value) => Some(value),
            Value::Bytes(_) => None,
        }
    }

    pub fn bytes(self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(value) => Some(value),
            Value::Int(_) => None,
        }
    }

    pub fn text(self) -> String {
        String::from_utf8_lossy(self.bytes().unwrap_or_default()).into_owned()
    }
}

fn take_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Fields of an encoded message, in order; stops at the first malformed one.
pub fn fields(mut input: &[u8]) -> impl Iterator<Item = (u32, Value<'_>)> {
    std::iter::from_fn(move || {
        let key = take_varint(&mut input)?;
        let number = u32::try_from(key >> 3).ok()?;
        let value = match key & 7 {
            0 => Value::Int(take_varint(&mut input)?),
            2 => {
                let length = usize::try_from(take_varint(&mut input)?).ok()?;
                let (value, rest) = input.split_at_checked(length)?;
                input = rest;
                Value::Bytes(value)
            }
            1 => {
                let (value, rest) = input.split_at_checked(8)?;
                input = rest;
                Value::Int(u64::from_le_bytes(value.try_into().ok()?))
            }
            5 => {
                let (value, rest) = input.split_at_checked(4)?;
                input = rest;
                Value::Int(u64::from(u32::from_le_bytes(value.try_into().ok()?)))
            }
            _ => return None,
        };
        Some((number, value))
    })
}

/// First value of a field.
pub fn field(input: &[u8], number: u32) -> Option<Value<'_>> {
    fields(input).find(|(found, _)| *found == number).map(|(_, value)| value)
}

/// Bytes received and not yet read: the connection is read with a timeout (to send
/// pings), so a message may arrive in several pieces.
#[derive(Default)]
pub struct Inbox(Vec<u8>);

impl Inbox {
    pub fn push(&mut self, data: &[u8]) {
        self.0.extend_from_slice(data);
    }

    /// Next complete message, if it has fully arrived.
    pub fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut input = &self.0[..];
        let Some(length) = take_varint(&mut input) else {
            return if self.0.len() >= 10 { Err(invalid("invalid length")) } else { Ok(None) };
        };
        if length > MAX_MESSAGE {
            return Err(invalid("message too large"));
        }
        let Some(message) = input.get(..length as usize) else { return Ok(None) };
        let message = message.to_vec();
        let used = self.0.len() - input.len() + message.len();
        self.0.drain(..used);
        Ok(Some(message))
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let inner = Message::new().uint(1, 300).text(2, "jf");
        let outer = Message::new().uint(1, 2).uint(2, 200).message(10, inner);
        let framed = outer.delimited();
        let mut inbox = Inbox::default();
        inbox.push(&framed[..3]);
        assert!(inbox.next().unwrap().is_none(), "partial message");
        inbox.push(&framed[3..]);
        let read = inbox.next().unwrap().unwrap();
        assert!(inbox.next().unwrap().is_none());
        assert_eq!(field(&read, 2), Some(Value::Int(200)));
        let inner = field(&read, 10).and_then(Value::bytes).unwrap();
        assert_eq!(field(inner, 1), Some(Value::Int(300)));
        assert_eq!(field(inner, 2).map(Value::text).as_deref(), Some("jf"));
        assert_eq!(fields(&[0x08]).count(), 0, "truncated message");
    }
}
