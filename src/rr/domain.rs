/*
 * Copyright (C) 2015 Benjamin Fry <benjaminfry@me.com>
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use std::ops::Index;

use ::serialize::binary::*;
use ::error::*;

/// TODO: all Names should be stored in a global "intern" space, and then everything that uses
///  them should be through references.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Name {
  labels: Vec<String>
}

impl Name {
  pub fn new() -> Self {
    Name { labels: Vec::new() }
  }

  // inline builder
  pub fn label(self, label: &'static str) -> Self {
    let mut me = self;
    me.labels.push(label.into());
    me
  }

  // for mutating over time
  pub fn with_labels(labels: Vec<String>) -> Self {
    Name { labels: labels }
  }

  pub fn add_label(&mut self, label: String) -> &mut Self {
    self.labels.push(label);
    self
  }

  pub fn append(&mut self, other: &Self) -> &mut Self {
    for s in &other.labels {
      self.add_label(s.to_string());
    }

    self
  }

  /// Trims off the first part of the name, to help with searching for the domain piece
  /// TODO: This makes a copy right now, probably can do something better...
  pub fn base_name(&self) -> Option<Name> {
    if self.labels.len() >= 1 {
      Some(Self::with_labels(self.labels[1..].to_vec()))
    } else {
      None
    }
  }

  // TODO: I think this does the wrong thing for escaped data
  pub fn parse(local: &str, origin: Option<&Self>) -> ParseResult<Self> {
    let mut build = Name::new();
    // split the local part

    // TODO: this should be a real lexer, to varify all data is legal name...
    for s in local.split('.') {
      if s.len() > 0 {
        build.add_label(s.to_string().to_lowercase()); // all names stored in lowercase
      }
    }

    if !local.ends_with('.') {
      build.append(try!(origin.ok_or(ParseError::OriginIsUndefined)));
    }

    Ok(build)
  }
}

impl BinSerializable for Name {
  /// parses the chain of labels
  ///  this has a max of 255 octets, with each label being less than 63.
  ///  all names will be stored lowercase internally.
  /// This will consume the portions of the Vec which it is reading...
  fn read(decoder: &mut BinDecoder) -> DecodeResult<Name> {
    let mut state: LabelParseState = LabelParseState::LabelLengthOrPointer;
    let mut labels: Vec<String> = Vec::with_capacity(3); // most labels will be around three, e.g. www.example.com

    // assume all chars are utf-8. We're doing byte-by-byte operations, no endianess issues...
    // reserved: (1000 0000 aka 0800) && (0100 0000 aka 0400)
    // pointer: (slice == 1100 0000 aka C0) & C0 == true, then 03FF & slice = offset
    // label: 03FF & slice = length; slice.next(length) = label
    // root: 0000
    loop {
      state = match state {
        LabelParseState::LabelLengthOrPointer => {
          // determine what the next label is
          match decoder.peek() {
            Some(0) | None => LabelParseState::Root,
            Some(byte) if byte & 0xC0 == 0xC0 => LabelParseState::Pointer,
            Some(byte) if byte <= 0x3F        => LabelParseState::Label,
            _ => unreachable!(),
          }
        },
        LabelParseState::Label => {
          labels.push(try!(decoder.read_character_data()));

          // reset to collect more data
          LabelParseState::LabelLengthOrPointer
        },
        //         4.1.4. Message compression
        //
        // In order to reduce the size of messages, the domain system utilizes a
        // compression scheme which eliminates the repetition of domain names in a
        // message.  In this scheme, an entire domain name or a list of labels at
        // the end of a domain name is replaced with a pointer to a prior occurance
        // of the same name.
        //
        // The pointer takes the form of a two octet sequence:
        //
        //     +--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+
        //     | 1  1|                OFFSET                   |
        //     +--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+
        //
        // The first two bits are ones.  This allows a pointer to be distinguished
        // from a label, since the label must begin with two zero bits because
        // labels are restricted to 63 octets or less.  (The 10 and 01 combinations
        // are reserved for future use.)  The OFFSET field specifies an offset from
        // the start of the message (i.e., the first octet of the ID field in the
        // domain header).  A zero offset specifies the first byte of the ID field,
        // etc.
        LabelParseState::Pointer => {
          let location = try!(decoder.read_u16()) & 0x3FFF; // get rid of the two high order bits
          let mut pointer = decoder.clone(location);
          let pointed = try!(Name::read(&mut pointer));

          for l in pointed.labels {
            labels.push(l);
          }

          // Pointers always finish the name, break like Root.
          break;
        },
        LabelParseState::Root => {
          // need to pop() the 0 off the stack...
          try!(decoder.pop());
          break;
        }
      }
    }

    Ok(Name { labels: labels })
  }

  fn emit(&self, encoder: &mut BinEncoder) -> EncodeResult {

    let buf_len = encoder.len(); // lazily assert the size is less than 255...
    for label in &self.labels {
      let label_len = encoder.len();
      try!(encoder.emit_character_data(label));

      // individual labels must be shorter than 63.
      let length = encoder.len() - label_len;
      if length > 63 { return Err(EncodeError::LabelBytesTooLong(length)); }
    }

    // the end of the list of names
    try!(encoder.emit(0));

     // the entire name needs to be less than 256.
    let length = encoder.len() - buf_len;
    if length > 255 { return Err(EncodeError::DomainNameTooLong(length)); }

    Ok(())
  }
}

impl Index<usize> for Name {
    type Output = String;

    fn index<'a>(&'a self, _index: usize) -> &'a String {
        &self.labels[_index]
    }
}

/// This is the list of states for the label parsing state machine
enum LabelParseState {
  LabelLengthOrPointer, // basically the start of the FSM
  Label,   // storing length of the label, must be < 63
  Pointer, // location of pointer in slice,
  Root,    // root is the end of the labels list, aka null
}

#[cfg(test)]
mod tests {
  use super::*;
  use ::serialize::binary::bin_tests::{test_read_data_set, test_emit_data_set};
  use ::serialize::binary::*;

  fn get_data() -> Vec<(Name, Vec<u8>)> {
    vec![
      (Name { labels: vec![] }, vec![0]), // base case, only the root
      (Name { labels: vec!["a".to_string()] }, vec![1,b'a',0]), // a single 'a' label
      (Name { labels: vec!["a".to_string(), "bc".to_string()] }, vec![1,b'a',2,b'b',b'c',0]), // two labels, 'a.bc'
      (Name { labels: vec!["a".to_string(), "♥".to_string()] }, vec![1,b'a',3,0xE2,0x99,0xA5,0]), // two labels utf8, 'a.♥'
    ]
  }

  #[test]
  fn parse() {
    test_read_data_set(get_data(), |ref mut d| Name::read(d));
  }

  #[test]
  fn write_to() {
    test_emit_data_set(get_data(), |e, n| n.emit(e));
  }
}