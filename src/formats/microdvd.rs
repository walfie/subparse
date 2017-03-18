// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use {ParseSubtitleString, SubtitleEntry, SubtitleFile};
use errors::Result as SubtitleParserResult;
use formats::common::*;
use timetypes::{TimePoint, TimeSpan};
use self::errors::ErrorKind::*;
use self::errors::*;

use std;
use std::collections::LinkedList;
use std::borrow::Cow;
use std::collections::HashSet;

use itertools::Itertools;

use combine::char::char;
use combine::combinator::{eof, many, parser as p, satisfy, sep_by, try};
use combine::primitives::{ParseError, ParseResult, Parser, Stream};

/// `.sub`(MicroDVD)-parser-specific errors
#[allow(missing_docs)]
pub mod errors {
    // see https://docs.rs/error-chain/0.8.1/error_chain/
    // this error type might be overkill, but that way it stays consistent with
    // the other parsers
    error_chain! {
        errors {
            LineParserError(line_num: usize, msg: String) {
                display("parse error at line `{}` because of `{}`", line_num, msg)
            }
            ErrorAtLine(line_num: usize) {
                display("parse error at line `{}`", line_num)
            }
        }
    }
}

/// Represents a formatting like "{y:i}" (display text in italics).
///
/// TODO: `MdvdFormatting` is a stub for the future where this enum holds specialized variants for different options.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum MdvdFormatting {
    /// A format option that is not directly supported.
    Unknown(String),
}

impl From<String> for MdvdFormatting {
    fn from(f: String) -> MdvdFormatting {
        MdvdFormatting::Unknown(Self::lowercase_first_char(&f))
    }
}

impl MdvdFormatting {
    /// Applies `to_lowercase()` to first char, leaves the rest of the characters untouched.
    fn lowercase_first_char(s: &str) -> String {
        let mut c = s.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        }
    }

    /// Applies `to_uppercase()` to first char, leaves the rest of the characters untouched.
    fn uppercase_first_char(s: &str) -> String {
        let mut c = s.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    }

    fn to_formatting_string_intern(&self) -> String {
        match *self {
            MdvdFormatting::Unknown(ref s) => s.clone(),
        }
    }

    ///
    fn to_formatting_string(&self, multiline: bool) -> String {
        let s = self.to_formatting_string_intern();
        match multiline {
            true => Self::uppercase_first_char(&s),
            false => Self::lowercase_first_char(&s),
        }
    }
}

#[derive(Debug, Clone)]
/// Represents a reconstructable `.sub`(MicroDVD) file.
pub struct MdvdFile {
    /// Number of frames per second of the accociated video (default 25)
    /// -> start/end frames can be coverted to timestamps
    fps: f64,

    /// all lines and multilines
    v: Vec<MdvdLine>,
}

/// Holds the description of a line like.
#[derive(Debug, Clone)]
struct MdvdLine {
    /// The start frame.
    start_frame: i64,

    /// The end frame.
    end_frame: i64,

    /// Formatting that affects all contained single lines.
    formatting: Vec<MdvdFormatting>,

    /// The (dialog) text of the line.
    text: String,
}

impl MdvdLine {
    fn to_subtitle_entry(&self, fps: f64) -> SubtitleEntry {
        SubtitleEntry {
            timespan: TimeSpan::new(TimePoint::from_msecs((self.start_frame as f64 * 1000.0 / fps) as i64),
                                    TimePoint::from_msecs((self.end_frame as f64 * 1000.0 / fps) as i64)),
            line: Some(self.text.clone()),
        }
    }
}

impl ParseSubtitleString for MdvdFile {
    fn parse_from_string(s: String) -> SubtitleParserResult<MdvdFile> {
        let file_opt = Self::parse_file(s.as_str());
        match file_opt {
            Ok(file) => Ok(file),
            Err(err) => Err(err.into()),
        }
    }
}

/// Implements parse functions.
impl MdvdFile {
    fn parse_file(i: &str) -> Result<MdvdFile> {
        let mut result: Vec<MdvdLine> = Vec::new();

        // remove utf-8 bom
        let (_, s) = split_bom(i);

        for (line_num, line) in s.lines().enumerate() {
            // a line looks like "{0}{25}{c:$0000ff}{y:b,u}{f:DeJaVuSans}{s:12}Hello!|{y:i}Hello2!" where
            // 0 and 25 are the start and end frames and the other information is the formatting.
            let mut lines: Vec<MdvdLine> = Self::get_line(line_num, &line)?;
            result.append(&mut lines);
        }

        Ok(MdvdFile {
            fps: 25.0,
            v: result,
        })
    }

    /// Matches a line in the text file which might correspond to multiple subtitle entries.
    fn get_line(line_num: usize, s: &str) -> Result<Vec<MdvdLine>> {
        Self::handle_error(p(Self::parse_container_line).parse(s), line_num)
    }

    /// Convert a result/error from the combine library to the srt parser error.
    fn handle_error<T>(r: std::result::Result<(T, &str), ParseError<&str>>, line_num: usize) -> Result<T> {
        r.map(|(v, _)| v)
         .map_err(|_| Error::from(ErrorAtLine(line_num)))
    }


    /// Matches the regex "\{[^}]*\}"; parses something like "{some_info}".
    fn parse_sub_info<I>(input: I) -> ParseResult<String, I>
        where I: Stream<Item = char>
    {
        (char('{'), many(satisfy(|c| c != '}')), char('}'))
            .map(|(_, info, _): (_, String, _)| info)
            .expected("MicroDVD info")
            .parse_stream(input)
    }

    // Parses something like "{C:$0000ff}{y:b,u}{f:DeJaVuSans}{s:12}Hello!"
    //
    // Returns the a tuple of the multiline-formatting, the single-line formatting and the text of the single line.
    fn parse_single_line<I>(input: I) -> ParseResult<(Vec<String>, String), I>
        where I: Stream<Item = char>
    {
        // the '|' char splits single lines
        (many(try(p(Self::parse_sub_info))), many(satisfy(|c| c != '|'))).parse_stream(input)
    }

    fn is_container_line_formatting(f: &String) -> bool {
        f.chars().next().and_then(|c| Some(c.is_uppercase())).unwrap_or(false)
    }

    // Parses something like "{0}{25}{C:$0000ff}{y:b,u}{f:DeJaVuSans}{s:12}Hello!|{s:15}Hello2!"
    fn parse_container_line<I>(input: I) -> ParseResult<Vec<MdvdLine>, I>
        where I: Stream<Item = char>
    {
        // the '|' char splits single lines
        (char('{'), p(number_i64), char('}'), char('{'), p(number_i64), char('}'), sep_by(p(Self::parse_single_line), char('|')), eof())
            .map(|(_, start_frame, _, _, end_frame, _, fmt_strs_and_lines, ()): (_, i64, _, _, i64, _, Vec<(Vec<String>, String)>, ())| {
                Self::construct_mdvd_lines(start_frame, end_frame, fmt_strs_and_lines)
            })
            .parse_stream(input)
    }

    /// Construct (possibly multiple) `MdvdLines` from a deconstructed file line
    /// like "{C:$0000ff}{y:b,u}{f:DeJaVuSans}{s:12}Hello!|{s:15}Hello2!".
    ///
    /// The third parameter is for the example
    /// like `[(["C:$0000ff", "y:b,u", "f:DeJaVuSans", "s:12"], "Hello!"), (["s:15"], "Hello2!")].
    fn construct_mdvd_lines(start_frame: i64, end_frame: i64, fmt_strs_and_lines: Vec<(Vec<String>, String)>) -> Vec<MdvdLine> {

        // saves all multiline formatting
        let mut cline_fmts: Vec<MdvdFormatting> = Vec::new();

        // convert the formatting strings to `MdvdFormatting` objects and split between multi-line and single-line formatting
        let fmts_and_lines: Vec<(Vec<MdvdFormatting>, String)> = fmt_strs_and_lines.into_iter()
                                                                                   .map(|(fmts, text)| {
            // split multiline-formatting (e.g "Y:b") and single-line formatting (e.g "y:b")
            let (cline_fmts_str, sline_fmts_str): (Vec<_>, Vec<_>) = fmts.into_iter().partition(Self::is_container_line_formatting);
            cline_fmts.extend(&mut cline_fmts_str.into_iter().map(MdvdFormatting::from));

            (sline_fmts_str.into_iter().map(MdvdFormatting::from).collect(), text)
        })
                                                                                   .collect();

        // now we also have all multi-line formattings
        fmts_and_lines.into_iter()
                      .map(|(sline_fmts, text)| {
            MdvdLine {
                start_frame: start_frame,
                end_frame: end_frame,
                text: text,
                formatting: cline_fmts.clone().into_iter().chain(sline_fmts.into_iter()).collect(),
            }
        })
                      .collect()
    }
}

impl SubtitleFile for MdvdFile {
    fn get_subtitle_entries(&self) -> SubtitleParserResult<Vec<SubtitleEntry>> {
        Ok(self.v
               .iter()
               .map(|line| line.to_subtitle_entry(self.fps))
               .collect())
    }

    fn update_subtitle_entries(&mut self, new_subtitle_entries: &[SubtitleEntry]) -> SubtitleParserResult<()> {
        assert_eq!(new_subtitle_entries.len(), self.v.len());

        let mut iter = new_subtitle_entries.iter().peekable();
        for line in &mut self.v {
            let peeked = iter.next().unwrap();

            line.start_frame = (peeked.timespan.start.secs_f64() * self.fps) as i64;
            line.end_frame = (peeked.timespan.end.secs_f64() * self.fps) as i64;

            if let Some(ref text) = peeked.line {
                line.text = text.clone();
            }
        }

        Ok(())
    }

    fn to_data(&self) -> SubtitleParserResult<Vec<u8>> {
        let mut sorted_list = self.v.clone();
        sorted_list.sort_by_key(|line| (line.start_frame, line.end_frame));

        let mut result: LinkedList<Cow<'static, str>> = LinkedList::new();

        for (gi, group_iter) in sorted_list.into_iter().group_by(|line| (line.start_frame, line.end_frame)).into_iter().enumerate() {
            if gi != 0 {
                result.push_back("\n".into());
            }

            let group: Vec<MdvdLine> = group_iter.1.collect();
            let group_len = group.len();

            let (start_frame, end_frame) = group_iter.0;
            let (formattings, texts): (Vec<HashSet<MdvdFormatting>>, Vec<String>) =
                group.into_iter()
                     .map(|line| (line.formatting.into_iter().collect(), line.text))
                     .unzip();

            // all single lines in the container line "cline" have the same start and end time
            //  -> the .sub file format let's them be on the same line with "{0}{1000}Text1|Text2"

            // find common formatting in all lines
            let common_formatting = if group_len == 1 {
                // if this "group" only has a single line, let's say that every formatting is individual
                HashSet::new()
            } else {
                formattings.iter()
                           .fold(None, |acc, set| match acc {
                               None => Some(set.clone()),
                               Some(acc_set) => Some(acc_set.intersection(&set).cloned().collect()),
                           })
                           .unwrap()
            };

            let individual_formattings = formattings.into_iter()
                                                    .map(|formatting| formatting.difference(&common_formatting).cloned().collect())
                                                    .collect::<Vec<HashSet<MdvdFormatting>>>();


            result.push_back("{".into());
            result.push_back(start_frame.to_string().into());
            result.push_back("}".into());

            result.push_back("{".into());
            result.push_back(end_frame.to_string().into());
            result.push_back("}".into());

            for formatting in &common_formatting {
                result.push_back("{".into());
                result.push_back(formatting.to_formatting_string(true).into());
                result.push_back("}".into());
            }

            for (i, (individual_formatting, text)) in individual_formattings.into_iter().zip(texts.into_iter()).enumerate() {
                if i != 0 {
                    result.push_back("|".into());
                }

                for formatting in individual_formatting {
                    result.push_back("{".into());
                    result.push_back(formatting.to_formatting_string(false).into());
                    result.push_back("}".into());
                }

                result.push_back(text.into());
            }


            // ends "group-by-frametime"-loop
        }

        Ok(result.into_iter().map(|cow| cow.to_string()).collect::<String>().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use SubtitleFile;
    use super::*;

    /// Parse string with `MdvdFile`, and reencode it with `MdvdFile`.
    fn mdvd_reconstruct(s: &str) -> String {
        let file = MdvdFile::parse_from_string(s.to_string()).unwrap();
        let data = file.to_data().unwrap();
        String::from_utf8(data).unwrap()
    }

    /// Parse and re-construct MicroDVD files and test them against expected output.
    fn test_mdvd(input: &str, expected: &str) {
        // if we put the `input` into the parser, we expect a specific (cleaned-up) output
        assert_eq!(mdvd_reconstruct(input), expected);

        // if we reconstuct he cleaned-up output, we expect that nothing changes
        assert_eq!(mdvd_reconstruct(expected), expected);
    }

    #[test]
    fn mdvd_test_reconstruction() {
        // simple examples
        test_mdvd("{0}{25}Hello!", "{0}{25}Hello!");
        test_mdvd("{0}{25}{y:i}Hello!", "{0}{25}{y:i}Hello!");
        test_mdvd("{0}{25}{Y:i}Hello!", "{0}{25}{y:i}Hello!");
        test_mdvd("{0}{25}{Y:i}\n", "{0}{25}{y:i}");

        // cleanup formattings in a file
        test_mdvd("{0}{25}{y:i}Text1|{y:i}Text2", "{0}{25}{Y:i}Text1|Text2");
        test_mdvd("{0}{25}{y:i}Text1\n{0}{25}{y:i}Text2",
                  "{0}{25}{Y:i}Text1|Text2");
        test_mdvd("{0}{25}{y:i}{y:b}Text1\n{0}{25}{y:i}Text2",
                  "{0}{25}{Y:i}{y:b}Text1|Text2");
        test_mdvd("{0}{25}{y:i}{y:b}Text1\n{0}{25}{y:i}Text2",
                  "{0}{25}{Y:i}{y:b}Text1|Text2");

        // these can't be condensed, because the lines have different times
        test_mdvd("{0}{25}{y:i}Text1\n{0}{26}{y:i}Text2",
                  "{0}{25}{y:i}Text1\n{0}{26}{y:i}Text2");
    }
}