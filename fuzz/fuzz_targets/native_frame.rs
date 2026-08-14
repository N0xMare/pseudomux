#![no_main]

use libfuzzer_sys::fuzz_target;
use pseudomux_protocol::v1::{
    EventEnvelope, MAX_NATIVE_FRAME_BYTES, NativeFrameAccumulator, NativeFrameAdmission,
    NativeFrameProgress, RequestEnvelope, ResponseEnvelope, admit_native_frame_header,
};
use serde::{Serialize, de::DeserializeOwned};

const MAX_FUZZ_INPUT_BYTES: usize = MAX_NATIVE_FRAME_BYTES;
const MAX_STREAM_FRAMES: usize = 64;
const MAX_SYNTHESIZED_MULTI_FRAME_BYTES: usize = 64 * 1024;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_FUZZ_INPUT_BYTES)];

    assert_frame_boundaries();

    // Raw JSON keeps the reviewed textual corpus useful. Every input is also
    // wrapped in one admitted frame so the production accumulator sees valid
    // header/payload fragmentation at arbitrary sizes. Finally, the raw bytes
    // are interpreted as a potentially partial or multi-frame stream.
    exercise_payload(input);

    let mut synthesized = Vec::with_capacity(input.len() + 4);
    synthesized.extend_from_slice(&(input.len() as u32).to_be_bytes());
    synthesized.extend_from_slice(input);
    let synthesized_outcome = decode_stream(&synthesized, usize::MAX);
    assert_eq!(
        synthesized_outcome.frames,
        vec![DecodedFrame::Payload(input.to_vec())]
    );
    assert_eq!(synthesized_outcome.consumed, synthesized.len());
    assert!(synthesized_outcome.accumulator.is_empty());
    assert_fragmentation_invariant(&synthesized, &synthesized_outcome);

    // Guarantee that every ordinarily sized fuzz case also traverses a
    // concatenated two-frame stream. Raw mutations remain useful for partial,
    // oversized, and longer arbitrary streams, but no corpus mutation is
    // required to reach the multi-frame state.
    if input.len() <= MAX_SYNTHESIZED_MULTI_FRAME_BYTES {
        let split = input
            .first()
            .map_or(0, |selector| usize::from(*selector) % (input.len() + 1));
        let (first, second) = input.split_at(split);
        let mut concatenated = Vec::with_capacity(input.len() + 8);
        concatenated.extend_from_slice(&(first.len() as u32).to_be_bytes());
        concatenated.extend_from_slice(first);
        concatenated.extend_from_slice(&(second.len() as u32).to_be_bytes());
        concatenated.extend_from_slice(second);
        let expected = StreamOutcome {
            frames: vec![
                DecodedFrame::Payload(first.to_vec()),
                DecodedFrame::Payload(second.to_vec()),
            ],
            accumulator: NativeFrameAccumulator::new(),
            consumed: concatenated.len(),
        };
        assert_eq!(decode_stream(&concatenated, usize::MAX), expected);
        assert_fragmentation_invariant(&concatenated, &expected);
    }

    let raw_outcome = decode_stream(input, usize::MAX);
    assert_fragmentation_invariant(input, &raw_outcome);
});

#[derive(Clone, Debug, Eq, PartialEq)]
enum DecodedFrame {
    Payload(Vec<u8>),
    Oversized(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamOutcome {
    frames: Vec<DecodedFrame>,
    accumulator: NativeFrameAccumulator,
    consumed: usize,
}

fn assert_fragmentation_invariant(stream: &[u8], expected: &StreamOutcome) {
    let fine_width = if stream.len() <= 64 * 1024 { 1 } else { 1_021 };
    for width in [fine_width, 3, 31, 8_191] {
        assert_eq!(decode_stream(stream, width), *expected);
    }
}

fn decode_stream(stream: &[u8], fragment_width: usize) -> StreamOutcome {
    let mut accumulator = NativeFrameAccumulator::new();
    let mut frames = Vec::new();
    let mut consumed = 0;
    while consumed < stream.len() && frames.len() < MAX_STREAM_FRAMES {
        let end = consumed.saturating_add(fragment_width).min(stream.len());
        let (fragment_consumed, progress) = accumulator.push(&stream[consumed..end]);
        assert!(fragment_consumed <= end - consumed);
        if end > consumed {
            assert!(fragment_consumed > 0);
        }
        consumed += fragment_consumed;
        match progress {
            NativeFrameProgress::NeedMore => {}
            NativeFrameProgress::Payload(payload) => {
                exercise_payload(&payload);
                frames.push(DecodedFrame::Payload(payload));
            }
            NativeFrameProgress::Oversized { advertised_bytes } => {
                frames.push(DecodedFrame::Oversized(advertised_bytes));
                // The daemon writes one bounded error and closes because the
                // unread advertised body cannot be resynchronized.
                break;
            }
        }
    }
    StreamOutcome {
        frames,
        accumulator,
        consumed,
    }
}

fn assert_frame_boundaries() {
    for payload_bytes in [0, MAX_NATIVE_FRAME_BYTES - 1, MAX_NATIVE_FRAME_BYTES] {
        assert_eq!(
            admit_native_frame_header((payload_bytes as u32).to_be_bytes()),
            NativeFrameAdmission::Payload { payload_bytes }
        );
    }
    let advertised_bytes = (MAX_NATIVE_FRAME_BYTES as u32) + 1;
    assert_eq!(
        admit_native_frame_header(advertised_bytes.to_be_bytes()),
        NativeFrameAdmission::Oversized { advertised_bytes }
    );
}

fn exercise_payload(payload: &[u8]) {
    round_trip::<RequestEnvelope>(payload);
    round_trip::<ResponseEnvelope>(payload);
    round_trip::<EventEnvelope>(payload);
}

fn round_trip<T>(payload: &[u8])
where
    T: DeserializeOwned + Serialize,
{
    let Ok(decoded) = serde_json::from_slice::<T>(payload) else {
        return;
    };
    let encoded = serde_json::to_vec(&decoded)
        .expect("a protocol DTO accepted from JSON must remain serializable");
    // A valid inbound request can omit defaulted fields. Canonical
    // reserialization may therefore be larger than its admitted input near
    // the transport ceiling; outbound framing owns that separate bound.
    let redecoded: T = serde_json::from_slice(&encoded)
        .expect("a serialized production protocol DTO must deserialize again");
    let reencoded = serde_json::to_vec(&redecoded)
        .expect("a round-tripped production protocol DTO must remain serializable");
    assert_eq!(reencoded, encoded);
}
