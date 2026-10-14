//! Operational transformation for plain text, for editing one file from several places
//! at once. An operation walks the whole document: it keeps (`Retain`), adds (`Insert`)
//! or removes (`Delete`) characters. Two operations made on the same text at the same
//! time can be transformed so that applying them in either order gives the same result.
//! Lengths count Unicode scalar values, never bytes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Retain(usize),
    Insert(String),
    Delete(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextOp {
    pub ops: Vec<Op>,
    /// Length of the text it applies to.
    pub base_len: usize,
    /// Length of the text it makes.
    pub target_len: usize,
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

impl TextOp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn retain(&mut self, n: usize) -> &mut Self {
        if n == 0 {
            return self;
        }
        self.base_len += n;
        self.target_len += n;
        if let Some(Op::Retain(last)) = self.ops.last_mut() {
            *last += n;
        } else {
            self.ops.push(Op::Retain(n));
        }
        self
    }

    pub fn insert(&mut self, s: &str) -> &mut Self {
        if s.is_empty() {
            return self;
        }
        self.target_len += chars(s);
        let len = self.ops.len();
        match self.ops.as_mut_slice() {
            [.., Op::Insert(last)] => last.push_str(s),
            // An insert always comes before a delete at the same place, so equal
            // operations look the same however they were built.
            [.., Op::Insert(before), Op::Delete(_)] => before.push_str(s),
            [.., Op::Delete(_)] => self.ops.insert(len - 1, Op::Insert(s.to_string())),
            _ => self.ops.push(Op::Insert(s.to_string())),
        }
        self
    }

    pub fn delete(&mut self, n: usize) -> &mut Self {
        if n == 0 {
            return self;
        }
        self.base_len += n;
        if let Some(Op::Delete(last)) = self.ops.last_mut() {
            *last += n;
        } else {
            self.ops.push(Op::Delete(n));
        }
        self
    }

    pub fn is_noop(&self) -> bool {
        self.ops.iter().all(|op| matches!(op, Op::Retain(_)))
    }

    /// The operation turning `old` into `new`: what lies between their common start and
    /// common end is replaced.
    pub fn diff(old: &str, new: &str) -> Self {
        Self::diff_near(old, new, usize::MAX)
    }

    /// The same, with the change placed no later than `near` where it could have been made
    /// in more than one place: a letter typed next to the same letter, an indented line
    /// added above another. Given where the cursor was, the change is where it was typed,
    /// so a cursor carried along by it lands where the typing did.
    pub fn diff_near(old: &str, new: &str, near: usize) -> Self {
        let a: Vec<char> = old.chars().collect();
        let b: Vec<char> = new.chars().collect();
        let prefix = a
            .iter()
            .zip(&b)
            .take_while(|(x, y)| x == y)
            .count()
            .min(near);
        let max_suffix = a.len().min(b.len()) - prefix;
        let suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take(max_suffix)
            .take_while(|(x, y)| x == y)
            .count();
        let mut op = Self::new();
        op.retain(prefix);
        op.delete(a.len() - prefix - suffix);
        op.insert(&b[prefix..b.len() - suffix].iter().collect::<String>());
        op.retain(suffix);
        op
    }

    pub fn apply(&self, text: &str) -> Result<String, String> {
        let source: Vec<char> = text.chars().collect();
        if source.len() != self.base_len {
            return Err(format!(
                "operation is for {} characters, the text has {}",
                self.base_len,
                source.len()
            ));
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    out.extend(&source[at..at + n]);
                    at += n;
                }
                Op::Insert(s) => out.push_str(s),
                Op::Delete(n) => at += n,
            }
        }
        Ok(out)
    }

    /// One operation doing `self` then `next`.
    pub fn compose(&self, next: &TextOp) -> Result<TextOp, String> {
        if self.target_len != next.base_len {
            return Err("operations don't follow each other".into());
        }
        let mut out = TextOp::new();
        let mut a = self.ops.iter().cloned();
        let mut b = next.ops.iter().cloned();
        let (mut x, mut y) = (a.next(), b.next());
        loop {
            match (&mut x, &mut y) {
                (None, None) => break,
                (Some(Op::Delete(n)), _) => {
                    out.delete(*n);
                    x = a.next();
                }
                (_, Some(Op::Insert(s))) => {
                    out.insert(s);
                    y = b.next();
                }
                (None, _) | (_, None) => return Err("operations of different lengths".into()),
                (Some(Op::Retain(n)), Some(Op::Retain(m))) => {
                    let k = (*n).min(*m);
                    out.retain(k);
                    (*n, *m) = (*n - k, *m - k);
                }
                (Some(Op::Retain(n)), Some(Op::Delete(m))) => {
                    let k = (*n).min(*m);
                    out.delete(k);
                    (*n, *m) = (*n - k, *m - k);
                }
                (Some(Op::Insert(s)), Some(Op::Retain(m))) => {
                    let len = chars(s);
                    let k = len.min(*m);
                    out.insert(&s.chars().take(k).collect::<String>());
                    *s = s.chars().skip(k).collect();
                    *m -= k;
                }
                (Some(Op::Insert(s)), Some(Op::Delete(m))) => {
                    let len = chars(s);
                    let k = len.min(*m);
                    *s = s.chars().skip(k).collect();
                    *m -= k;
                }
            }
            if matches!(&x, Some(Op::Retain(0) | Op::Delete(0)))
                || matches!(&x, Some(Op::Insert(s)) if s.is_empty())
            {
                x = a.next();
            }
            if matches!(&y, Some(Op::Retain(0) | Op::Delete(0)))
                || matches!(&y, Some(Op::Insert(s)) if s.is_empty())
            {
                y = b.next();
            }
        }
        Ok(out)
    }

    /// Two operations made on the same text: returns `(a', b')` such that `a` then `b'`
    /// and `b` then `a'` give the same text. On inserts at the same place, `a`'s comes first.
    pub fn transform(a: &TextOp, b: &TextOp) -> Result<(TextOp, TextOp), String> {
        if a.base_len != b.base_len {
            return Err("operations aren't on the same text".into());
        }
        let (mut a2, mut b2) = (TextOp::new(), TextOp::new());
        let mut ia = a.ops.iter().cloned();
        let mut ib = b.ops.iter().cloned();
        let (mut x, mut y) = (ia.next(), ib.next());
        loop {
            match (&mut x, &mut y) {
                (None, None) => break,
                (Some(Op::Insert(s)), _) => {
                    a2.insert(s);
                    b2.retain(chars(s));
                    x = ia.next();
                    continue;
                }
                (_, Some(Op::Insert(s))) => {
                    a2.retain(chars(s));
                    b2.insert(s);
                    y = ib.next();
                    continue;
                }
                (None, _) | (_, None) => return Err("operations of different lengths".into()),
                (Some(Op::Retain(n)), Some(Op::Retain(m))) => {
                    let k = (*n).min(*m);
                    a2.retain(k);
                    b2.retain(k);
                    (*n, *m) = (*n - k, *m - k);
                }
                (Some(Op::Delete(n)), Some(Op::Delete(m))) => {
                    let k = (*n).min(*m);
                    (*n, *m) = (*n - k, *m - k);
                }
                (Some(Op::Delete(n)), Some(Op::Retain(m))) => {
                    let k = (*n).min(*m);
                    a2.delete(k);
                    (*n, *m) = (*n - k, *m - k);
                }
                (Some(Op::Retain(n)), Some(Op::Delete(m))) => {
                    let k = (*n).min(*m);
                    b2.delete(k);
                    (*n, *m) = (*n - k, *m - k);
                }
            }
            if matches!(&x, Some(Op::Retain(0) | Op::Delete(0))) {
                x = ia.next();
            }
            if matches!(&y, Some(Op::Retain(0) | Op::Delete(0))) {
                y = ib.next();
            }
        }
        Ok((a2, b2))
    }

    /// Where a place in the text before the operation ends up after it. An insert right
    /// at the place pushes it along unless `stay` is set.
    pub fn map(&self, at: usize, stay: bool) -> usize {
        let (mut old, mut new) = (0, 0);
        for op in &self.ops {
            if old > at || (old == at && stay) {
                break;
            }
            match op {
                Op::Retain(n) => {
                    if old + n > at {
                        return new + (at - old);
                    }
                    old += n;
                    new += n;
                }
                Op::Insert(s) => new += chars(s),
                Op::Delete(n) => {
                    if old + n > at {
                        return new;
                    }
                    old += n;
                }
            }
        }
        new + at.saturating_sub(old)
    }
}

/// The editing side of one shared document: its own edits wait for the broker's
/// acknowledgement one at a time, and edits from others are transformed past them.
#[derive(Clone, Debug, Default)]
pub struct Client {
    /// Operations the broker has applied, as far as this side knows.
    pub revision: u64,
    /// Sent and not yet acknowledged.
    pub outstanding: Option<TextOp>,
    /// Made while one was outstanding, to be sent after.
    pub buffer: Option<TextOp>,
}

impl Client {
    pub fn new(revision: u64) -> Self {
        Self {
            revision,
            ..Self::default()
        }
    }

    /// A local edit. Returns what to send now, if anything.
    pub fn local(&mut self, op: TextOp) -> Result<Option<(u64, TextOp)>, String> {
        if self.outstanding.is_none() {
            self.outstanding = Some(op.clone());
            return Ok(Some((self.revision, op)));
        }
        self.buffer = Some(match self.buffer.take() {
            Some(buffer) => buffer.compose(&op)?,
            None => op,
        });
        Ok(None)
    }

    /// The broker took our outstanding edit. Returns the next one to send, if any.
    pub fn ack(&mut self) -> Option<(u64, TextOp)> {
        self.revision += 1;
        self.outstanding = self.buffer.take();
        self.outstanding.clone().map(|op| (self.revision, op))
    }

    /// A place in the broker's text (after every edit it has passed on) as a place in the
    /// text here, which also has this side's edits not yet taken in.
    pub fn to_local(&self, at: usize) -> usize {
        let at = self.outstanding.as_ref().map_or(at, |op| op.map(at, false));
        self.buffer.as_ref().map_or(at, |op| op.map(at, false))
    }

    /// An edit from someone else. Returns it as it applies to the text here.
    pub fn remote(&mut self, op: TextOp) -> Result<TextOp, String> {
        self.revision += 1;
        let mut op = op;
        if let Some(outstanding) = self.outstanding.take() {
            let (o2, op2) = TextOp::transform(&outstanding, &op)?;
            self.outstanding = Some(o2);
            op = op2;
        }
        if let Some(buffer) = self.buffer.take() {
            let (b2, op2) = TextOp::transform(&buffer, &op)?;
            self.buffer = Some(b2);
            op = op2;
        }
        Ok(op)
    }
}

/// Someone editing, as others see them.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Peer {
    pub login: String,
    pub name: String,
    pub avatar: String,
}

/// From an editor to the broker, one JSON object per line.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToBroker {
    /// Start sharing a document; `text` is used if nobody has it open yet.
    Join {
        doc: String,
        user: Peer,
        text: String,
    },
    Leave {
        doc: String,
    },
    Op {
        doc: String,
        rev: u64,
        op: TextOp,
        /// The sender's cursor and selection end in the text the edit makes, so the others
        /// see the cursor move with the text, in one step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<(usize, usize)>,
    },
    Cursor {
        doc: String,
        at: usize,
        anchor: usize,
    },
    Saved {
        doc: String,
    },
}

/// From the broker to an editor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromBroker {
    /// The shared text, and who else has it open.
    Doc {
        doc: String,
        rev: u64,
        text: String,
        peers: Vec<(u64, Peer)>,
    },
    Ack {
        doc: String,
    },
    Op {
        doc: String,
        op: TextOp,
        /// Who made it, and where their cursor is after it (in the broker's text).
        #[serde(default)]
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<(usize, usize)>,
    },
    Joined {
        doc: String,
        id: u64,
        user: Peer,
    },
    Left {
        doc: String,
        id: u64,
    },
    Cursor {
        doc: String,
        id: u64,
        at: usize,
        anchor: usize,
    },
    Saved {
        doc: String,
        id: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A types quickly while edits are still on their way (acknowledgements come late);
    /// B watches. A's cursor, as B places it from each edit, only ever moves forward and
    /// ends where A's is — however many letters repeat.
    #[test]
    fn a_cursor_carried_with_the_edits_never_steps_back() {
        let start = "    hello\n    world\n";
        let mut a_text = start.to_string();
        let mut a_cursor = 9; // after "    hello"
        let mut a = Client::new(0);
        let mut b = Client::new(0);
        let mut b_text = start.to_string();
        let mut b_sees = a_cursor;
        // What the broker passes on to B, and the acks owed to A, in order.
        let (mut history, mut server) = (Vec::<TextOp>::new(), start.to_string());
        let mut to_b: Vec<(TextOp, (usize, usize))> = Vec::new();
        let mut acks = 0;
        let mut in_flight: Vec<(u64, TextOp, (usize, usize))> = Vec::new();
        let typed = ["o", "o", "\n", " ", " ", " ", " ", "l", "l", "o", "\n"];
        for (step, piece) in typed.iter().enumerate() {
            let old = a_text.clone();
            let mut chars: Vec<char> = a_text.chars().collect();
            for (i, c) in piece.chars().enumerate() {
                chars.insert(a_cursor + i, c);
            }
            a_text = chars.into_iter().collect();
            let near = a_cursor;
            a_cursor += piece.chars().count();
            let op = TextOp::diff_near(&old, &a_text, near);
            if let Some((rev, op)) = a.local(op).unwrap() {
                in_flight.push((rev, op, (a_cursor, a_cursor)));
            }
            // Every other keystroke, the broker catches up with what A sent.
            if step % 2 == 1 {
                for (rev, op, at) in in_flight.drain(..) {
                    let mut op = op;
                    let mut at = at;
                    for done in &history[rev as usize..] {
                        let (mine, theirs) = TextOp::transform(&op, done).unwrap();
                        op = mine;
                        at = (theirs.map(at.0, false), theirs.map(at.1, false));
                    }
                    server = op.apply(&server).unwrap();
                    history.push(op.clone());
                    to_b.push((op, at));
                    acks += 1;
                }
                // B takes in what reached it.
                for (op, at) in to_b.drain(..) {
                    let op = b.remote(op).unwrap();
                    b_text = op.apply(&b_text).unwrap();
                    let placed = b.to_local(at.0);
                    // Carried along by the edit alone, the cursor lands there too: the edit
                    // was placed where it was typed.
                    assert_eq!(op.map(b_sees, false), placed);
                    assert!(
                        placed >= b_sees,
                        "A's cursor stepped back: {b_sees} → {placed}"
                    );
                    b_sees = placed;
                }
                // A hears its edits were taken in; the next waiting one goes out.
                for _ in 0..std::mem::take(&mut acks) {
                    if let Some((rev, op)) = a.ack() {
                        in_flight.push((rev, op, (a_cursor, a_cursor)));
                    }
                }
            }
        }
        // The rest goes through.
        while !in_flight.is_empty() {
            for (rev, op, at) in in_flight.drain(..).collect::<Vec<_>>() {
                assert_eq!(rev as usize, history.len());
                server = op.apply(&server).unwrap();
                history.push(op.clone());
                let op = b.remote(op).unwrap();
                b_text = op.apply(&b_text).unwrap();
                let placed = b.to_local(at.0);
                assert!(placed >= b_sees);
                b_sees = placed;
                if let Some((rev, op)) = a.ack() {
                    in_flight.push((rev, op, (a_cursor, a_cursor)));
                }
            }
        }
        assert_eq!(b_text, a_text);
        assert_eq!(server, a_text);
        assert_eq!(b_sees, a_cursor);
    }

    #[test]
    fn a_letter_typed_next_to_the_same_letter_is_where_it_was_typed() {
        // "hel|lo" + 'l': plain diff puts the new 'l' after both, diff_near at the cursor.
        let op = TextOp::diff_near("hello", "helllo", 3);
        assert_eq!(op.apply("hello").unwrap(), "helllo");
        assert_eq!(op.map(3, false), 4);
        // Enter on an indented line above another indented line.
        let (old, new) = ("    a\n    b", "    a\n    \n    b");
        let op = TextOp::diff_near(old, new, 5);
        assert_eq!(op.apply(old).unwrap(), new);
        assert_eq!(op.map(5, false), 10);
        // Backspace over the first of two equal letters.
        let op = TextOp::diff_near("xaay", "xay", 1);
        assert_eq!(op.apply("xaay").unwrap(), "xay");
        assert_eq!(op.map(2, false), 1);
    }

    #[test]
    fn a_diff_turns_one_text_into_the_other() {
        for (old, new) in [
            ("hello world", "hello brave world"),
            ("abc", ""),
            ("", "xyz"),
            ("ăâî", "ăîș"),
            ("same", "same"),
        ] {
            assert_eq!(TextOp::diff(old, new).apply(old).unwrap(), new);
        }
    }

    #[test]
    fn concurrent_edits_meet_in_the_same_text() {
        let base = "The quick brown fox";
        let cases = [
            ("The quick brown fox jumps", "The very quick brown fox"),
            ("The brown fox", "The quick red fox"),
            ("The quick brown cat", "The quick brown dog"),
            ("", "The quick brown fox!"),
            ("Xhe quick brown fox", "The quick brown foY"),
        ];
        for (one, two) in cases {
            let a = TextOp::diff(base, one);
            let b = TextOp::diff(base, two);
            let (a2, b2) = TextOp::transform(&a, &b).unwrap();
            let left = b2.apply(&a.apply(base).unwrap()).unwrap();
            let right = a2.apply(&b.apply(base).unwrap()).unwrap();
            assert_eq!(left, right, "{one:?} / {two:?}");
        }
    }

    /// Many random pairs of edits, made the way an editor makes them (one region each).
    #[test]
    fn random_edits_always_meet() {
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        let alphabet: Vec<char> = "ab \nșă{}".chars().collect();
        let edit = |text: &str, next: &mut dyn FnMut(usize) -> usize| {
            let chars: Vec<char> = text.chars().collect();
            let start = next(chars.len() + 1);
            let end = start + next(chars.len() - start + 1);
            let insert: String = (0..next(4))
                .map(|_| alphabet[next(alphabet.len())])
                .collect();
            let mut out: String = chars[..start].iter().collect();
            out.push_str(&insert);
            out.extend(&chars[end..]);
            out
        };
        for _ in 0..500 {
            let base: String = (0..next(12))
                .map(|_| alphabet[next(alphabet.len())])
                .collect();
            let one = edit(&base, &mut next);
            let two = edit(&base, &mut next);
            let a = TextOp::diff(&base, &one);
            let b = TextOp::diff(&base, &two);
            let (a2, b2) = TextOp::transform(&a, &b).unwrap();
            assert_eq!(
                b2.apply(&one).unwrap(),
                a2.apply(&two).unwrap(),
                "{base:?} → {one:?} / {two:?}"
            );
            let three = edit(&one, &mut next);
            let c = TextOp::diff(&one, &three);
            assert_eq!(a.compose(&c).unwrap().apply(&base).unwrap(), three);
        }
    }

    #[test]
    fn composed_edits_do_both() {
        let a = TextOp::diff("abc", "abXc");
        let b = TextOp::diff("abXc", "YabXc");
        assert_eq!(a.compose(&b).unwrap().apply("abc").unwrap(), "YabXc");
    }

    #[test]
    fn places_follow_the_text() {
        let op = TextOp::diff("hello world", "hello brave world");
        assert_eq!(op.map(8, false), 14);
        assert_eq!(op.map(2, false), 2);
    }

    /// Two editors and a broker, edits crossing on the way: everyone ends with the same text.
    #[test]
    fn two_clients_and_a_broker_agree() {
        let mut server_text = String::from("let x = 1;");
        let mut history: Vec<TextOp> = Vec::new();
        let (mut text_a, mut text_b) = (server_text.clone(), server_text.clone());
        let (mut a, mut b) = (Client::new(0), Client::new(0));

        // Both edit before hearing of the other.
        let op_a = TextOp::diff(&text_a, "let xy = 1;");
        text_a = op_a.apply(&text_a).unwrap();
        let sent_a = a.local(op_a).unwrap().unwrap();
        let op_b = TextOp::diff(&text_b, "let x = 12;");
        text_b = op_b.apply(&text_b).unwrap();
        let sent_b = b.local(op_b).unwrap().unwrap();
        // A types more while waiting.
        let op_a2 = TextOp::diff(&text_a, "const xy = 1;");
        text_a = op_a2.apply(&text_a).unwrap();
        assert!(a.local(op_a2).unwrap().is_none());

        let mut receive = |rev: u64, op: TextOp| {
            let mut op = op;
            for done in &history[rev as usize..] {
                op = TextOp::transform(&op, done).unwrap().0;
            }
            server_text = op.apply(&server_text).unwrap();
            history.push(op.clone());
            op
        };
        // The broker gets A's first edit, then B's.
        let applied_a = receive(sent_a.0, sent_a.1);
        let applied_b = receive(sent_b.0, sent_b.1);
        // A hears its ack, then B's edit; B hears A's edit, then its ack.
        let next = a.ack().unwrap();
        text_a = a.remote(applied_b).unwrap().apply(&text_a).unwrap();
        text_b = b.remote(applied_a).unwrap().apply(&text_b).unwrap();
        assert!(b.ack().is_none());
        // A's buffered edit goes out and comes back to B.
        let applied_a2 = receive(next.0, next.1);
        assert!(a.ack().is_none());
        text_b = b.remote(applied_a2).unwrap().apply(&text_b).unwrap();

        assert_eq!(text_a, server_text);
        assert_eq!(text_b, server_text);
        assert_eq!(server_text, "const xy = 12;");
    }
}
