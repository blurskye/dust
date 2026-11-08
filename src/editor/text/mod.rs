//! Text storage and positions.

#![forbid(unsafe_code)]

mod codec;
mod delta;
mod display;
mod edit;
mod encoding;
mod grapheme;
mod position;
mod rope;

pub use codec::PositionCodec;

pub use codec::PositionEncoding;

pub use codec::TextPoint;
pub use delta::Affinity;
pub use delta::DeltaSpan;
pub use delta::EditDelta;
pub use display::DisplaySpan;
pub use display::DisplayWidthPolicy;
pub use display::byte_range_for_display_columns;
pub use display::display_column_at;
pub use display::display_spans;
pub use display::grapheme_cell_width;
pub use display::insertion_at_display_column;
pub use edit::EditError;
pub use edit::TextEdit;
pub use edit::TextEditSet;
pub use encoding::DecodedText;
pub use encoding::EncodingError;
pub use encoding::FileFormat;
pub use encoding::LineEnding;
pub use encoding::decode_utf8;
pub use encoding::encode_utf8;
pub use grapheme::display_width;
pub use grapheme::next_grapheme_boundary;
pub use grapheme::previous_grapheme_boundary;
pub use position::BufferRevision;
pub use position::ByteOffset;
pub use position::ByteRange;
pub use position::PositionError;
pub use rope::TextBuffer;
pub(crate) use rope::TextVersion;

pub(crate) mod diff;
