#![forbid(unsafe_code)]

mod codec;
mod delta;
mod display;
mod edit;
mod encoding;
mod grapheme;
mod position;
mod rope;

pub use codec::{PositionCodec, PositionEncoding, TextPoint};
pub use delta::{Affinity, DeltaSpan, EditDelta};
pub use display::{
    DisplaySpan, DisplayWidthPolicy, byte_range_for_display_columns, display_column_at,
    display_spans, grapheme_cell_width, insertion_at_display_column,
};
pub use edit::{EditError, TextEdit, TextEditSet};
pub use encoding::{DecodedText, EncodingError, FileFormat, LineEnding, decode_utf8, encode_utf8};
pub use grapheme::{display_width, next_grapheme_boundary, previous_grapheme_boundary};
pub use position::{BufferRevision, ByteOffset, ByteRange, PositionError};
pub use rope::TextBuffer;
