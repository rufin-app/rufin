//! A track's rating in its file's tags, in half stars from 1 to 10. Written as FMPS_RATING,
//! a fraction of one, which fooyin and Strawberry read in every tag format, and as POPM
//! beside it in ID3v2. Read from those and from the tags other players write.

use lofty::ape::{ApeItem, ApeTag};
use lofty::id3::v2::{Frame, Id3v2Tag, PopularimeterFrame};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::ItemValue;

use super::tags::RawTags;

const FMPS: &str = "FMPS_RATING";
const ID3_FMPS: &str = "FMPS_Rating";
const MP4_MEAN: &str = "com.apple.iTunes";

/// POPM's byte for each half star, as MediaMonkey and fooyin write it.
const POPM: [u8; 10] = [13, 1, 54, 64, 118, 128, 186, 196, 242, 255];

/// The rating a scan reads from a file's tags.
pub(crate) fn read(raw: &RawTags) -> Option<u8> {
    let first = |key: &str| raw.get(key).and_then(|values| values.first());
    let fmps = first("fmps_rating")
        .or_else(|| first("----:com.apple.itunes:fmps_rating"))
        .and_then(|value| value.parse::<f64>().ok())
        .and_then(half_stars);
    let popm = || first("popm").and_then(|value| popm(value.parse().ok()?));
    // A rating beside the name of who gave it, as `RATING:name` in Vorbis comments.
    let named = || {
        raw.iter()
            .find(|(key, _)| key.starts_with("rating:"))
            .and_then(|(_, values)| values.first())
    };
    fmps.or_else(popm).or_else(|| {
        first("rating")
            .or_else(named)
            .or_else(|| first("rate"))
            .and_then(|value| guessed(value))
    })
}

fn half_stars(fraction: f64) -> Option<u8> {
    let rating = (fraction * 10.0).round();
    (1.0..=10.0).contains(&rating).then_some(rating as u8)
}

/// A POPM byte as half stars, by the ranges MediaMonkey and fooyin read.
fn popm(byte: u8) -> Option<u8> {
    Some(match byte {
        0 => return None,
        1 => 2,
        2..=22 => 1,
        23..=31 => 2,
        32..=63 => 3,
        64..=95 => 4,
        96..=127 => 5,
        128..=159 => 6,
        160..=195 => 7,
        196..=223 => 8,
        224..=254 => 9,
        255 => 10,
    })
}

/// A rating whose tag names no scale: a fraction of one, stars out of five, points out of
/// ten or a percentage. `Name|stars|plays` is what Rufin wrote in Vorbis comments.
fn guessed(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(stars) = value.split('|').nth(1) {
        return half_stars(stars.trim().parse::<f64>().ok()? / 5.0);
    }
    let number = value.parse::<f64>().ok()?;
    half_stars(if number <= 1.0 && value.contains('.') {
        number
    } else if number <= 5.0 {
        number / 5.0
    } else if number <= 10.0 {
        number / 10.0
    } else {
        number / 100.0
    })
}

fn fmps(rating: u8) -> String {
    format!("{:.1}", f64::from(rating.min(10)) / 10.0)
}

fn is_rating_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key == FMPS || key == "RATING" || key.starts_with("RATING:")
}

/// Every POPM frame takes the rating and keeps its play counter, so no reader finds an old
/// value under another name.
pub(crate) fn write_id3v2(tag: &mut Id3v2Tag, rating: Option<u8>) {
    let byte = rating.map_or(0, |rating| POPM[usize::from(rating.clamp(1, 10)) - 1]);
    let mut found = false;
    tag.retain_mut(|frame| {
        let Frame::Popularimeter(frame) = frame else {
            return true;
        };
        found = true;
        frame.rating = byte;
        byte > 0 || frame.counter > 0
    });
    if !found && byte > 0 {
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new("", byte, 0)));
    }
    tag.remove_user_text(ID3_FMPS);
    if let Some(rating) = rating {
        tag.insert_user_text(ID3_FMPS.into(), fmps(rating));
    }
}

/// Lofty's shared tag holds a rating as whole stars and writes it back in its own scale.
/// These give a tag that an edit leaves the rating of the values `file` had, by the name
/// each is under.
pub(crate) fn keep_id3v2(tag: &mut Id3v2Tag, file: &RawTags) {
    tag.retain_mut(|frame| {
        if let Frame::Popularimeter(frame) = frame
            && let Some(byte) =
                kept(file, &format!("popm:{}", frame.email)).and_then(|byte| byte.parse().ok())
        {
            frame.rating = byte;
        }
        true
    });
}

pub(crate) fn keep_vorbis(tag: &mut VorbisComments, file: &RawTags) {
    let items = tag.take_items().collect::<Vec<_>>();
    for (key, value) in items {
        let had = key
            .to_ascii_uppercase()
            .starts_with("RATING:")
            .then(|| kept(file, &key))
            .flatten();
        tag.push(key, had.cloned().unwrap_or(value));
    }
}

pub(crate) fn keep_ilst(tag: &mut Ilst, file: &RawTags) {
    let rate = AtomIdent::Fourcc(*b"rate");
    if tag.get(&rate).is_some()
        && let Some(had) = kept(file, "rate")
    {
        tag.remove(&rate).for_each(drop);
        tag.insert(Atom::new(rate, AtomData::UTF8(had.clone())));
    }
}

fn kept<'a>(file: &'a RawTags, key: &str) -> Option<&'a String> {
    file.get(&key.to_lowercase())
        .and_then(|values| values.first())
}

pub(crate) fn write_vorbis(tag: &mut VorbisComments, rating: Option<u8>) {
    let kept = tag
        .take_items()
        .filter(|(key, _)| !is_rating_key(key))
        .collect::<Vec<_>>();
    for (key, value) in kept {
        tag.push(key, value);
    }
    if let Some(rating) = rating {
        tag.push(FMPS.into(), fmps(rating));
    }
}

pub(crate) fn write_ape(tag: &mut ApeTag, rating: Option<u8>) {
    tag.remove(FMPS);
    tag.remove("RATING");
    if let Some(rating) = rating {
        if let Ok(item) = ApeItem::new(FMPS.into(), ItemValue::Text(fmps(rating))) {
            tag.insert(item);
        }
    }
}

pub(crate) fn write_ilst(tag: &mut Ilst, rating: Option<u8>) {
    let freeform = AtomIdent::Freeform {
        mean: MP4_MEAN.into(),
        name: ID3_FMPS.into(),
    };
    tag.remove(&AtomIdent::Fourcc(*b"rate")).for_each(drop);
    tag.remove(&freeform).for_each(drop);
    if let Some(rating) = rating {
        tag.insert(Atom::new(freeform, AtomData::UTF8(fmps(rating))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> RawTags {
        let mut raw = RawTags::new();
        for (key, value) in pairs {
            super::super::tags::push(&mut raw, key, value);
        }
        raw
    }

    #[test]
    fn every_half_star_reads_back_from_its_popm_byte() {
        for rating in 1..=10 {
            assert_eq!(popm(POPM[usize::from(rating) - 1]), Some(rating));
        }
    }

    #[test]
    fn ratings_of_other_players_read_in_their_scale() {
        for (pairs, rating) in [
            (&[("FMPS_RATING", "0.7")][..], Some(7)),
            (&[("FMPS_RATING", "0.3"), ("RATING", "5")], Some(3)),
            (&[("popm", "196")], Some(8)),
            (&[("RATING", "1")], Some(2)),
            (&[("RATING", "4.5")], Some(9)),
            (&[("RATING", "0.5")], Some(5)),
            (&[("RATING", "80")], Some(8)),
            (&[("RATING:MusicBee", "60")], Some(6)),
            (&[("RATING", "MusicBee|4|0")], Some(8)),
            (&[("rate", "100")], Some(10)),
            (&[("RATING", "0")], None),
            (&[("title", "Song")], None),
        ] {
            assert_eq!(read(&raw(pairs)), rating, "{pairs:?}");
        }
    }
}
