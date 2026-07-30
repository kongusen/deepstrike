//! Cross-language scalar and projection rules (spec §7.1.1).
//!
//! Every rule here exists because the same value has to mean the same thing in Rust, Python,
//! Node and WASM:
//!
//! * logical `u64` travels as a **canonical decimal string** ([`WireU64`]) so a JS number can
//!   never silently round a step sequence or a millisecond clock;
//! * authoritative policy ratios travel as **fixed-point parts-per-million** ([`Ppm`]) so no
//!   branch depends on a language's default float;
//! * observation-only floats are [`FiniteF64`] — NaN/Infinity are rejected at the boundary;
//! * canonical bytes are [`CanonicalBytes`], whose JSON projection is an **explicit** base64
//!   envelope rather than a bare string or a number array;
//! * identities, digests and opaque references are branded newtypes, never bare integers.

use std::fmt;

use serde::de::{self, Deserializer, Unexpected, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// Prefix of every scalar-rule rejection. `WireRejection` classifies on it, so all four host
/// languages can map "this value broke a scalar rule" onto one structured error.
pub const SCALAR_ERROR_MARKER: &str = "wire scalar rejected";

/// Absolute byte bound for any branded identity on the wire.
pub const MAX_ID_BYTES: usize = 256;

/// Largest integer a IEEE-754 double represents exactly. `WireU64` values above it are exactly
/// the reason logical `u64` never travels as a JSON number.
pub const JS_SAFE_INTEGER_MAX: u64 = (1 << 53) - 1;

/// A scalar rejected by a wire rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireScalarError {
    pub message: String,
}

impl WireScalarError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WireScalarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCALAR_ERROR_MARKER}: {}", self.message)
    }
}

impl std::error::Error for WireScalarError {}

fn scalar_error<E: de::Error>(message: impl fmt::Display) -> E {
    E::custom(format!("{SCALAR_ERROR_MARKER}: {message}"))
}

// ---------------------------------------------------------------------------------------------
// WireU64
// ---------------------------------------------------------------------------------------------

/// A logical `u64` that travels as a canonical decimal string.
///
/// Canonical means: ASCII digits only, no sign, no radix prefix, no surrounding whitespace and
/// no leading zeros (`"0"` is the only representation of zero). Two hosts that mean the same
/// number therefore always produce the same bytes — a precondition for canonical record digests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireU64(u64);

impl WireU64 {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Whether this value survives a round-trip through a JS `number`. Hosts that project to
    /// `bigint` or keep the branded decimal string never need to ask.
    pub const fn is_js_safe(self) -> bool {
        self.0 <= JS_SAFE_INTEGER_MAX
    }

    pub fn parse(text: &str) -> Result<Self, WireScalarError> {
        if text.is_empty() {
            return Err(WireScalarError::new("u64 decimal string is empty"));
        }
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(WireScalarError::new(format!(
                "u64 must be a canonical decimal string, got {text:?}"
            )));
        }
        if text.len() > 1 && text.starts_with('0') {
            return Err(WireScalarError::new(format!(
                "u64 decimal string must not have leading zeros, got {text:?}"
            )));
        }
        text.parse::<u64>().map(Self).map_err(|_| {
            WireScalarError::new(format!("u64 decimal string {text:?} is out of range"))
        })
    }
}

impl fmt::Display for WireU64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for WireU64 {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for WireU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

struct WireU64Visitor;

impl Visitor<'_> for WireU64Visitor {
    type Value = WireU64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a canonical decimal string encoding a u64")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        WireU64::parse(value).map_err(|err| scalar_error(err.message))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "u64 must be a decimal string, got the JSON number {value}"
        )))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "u64 must be a decimal string, got the JSON number {value}"
        )))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "u64 must be a decimal string, got the JSON number {value}"
        )))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "u64 must be a decimal string, got the boolean {value}"
        )))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(scalar_error("u64 must be a decimal string, got null"))
    }
}

impl<'de> Deserialize<'de> for WireU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `deserialize_any` (not `deserialize_str`): serde_json answers a non-string token to
        // `deserialize_str` with its own `invalid_type` error before the visitor ever runs, which
        // would hide the scalar rule behind a generic type error.
        deserializer.deserialize_any(WireU64Visitor)
    }
}

// ---------------------------------------------------------------------------------------------
// Ppm
// ---------------------------------------------------------------------------------------------

/// A ratio in `[0, 1]` expressed as fixed-point parts-per-million.
///
/// Authoritative policy thresholds must not be floats: `0.25` is not representable identically in
/// every language/serializer, and a threshold comparison that differs by one ULP is a different
/// kernel decision. `Ppm(250_000)` is exact everywhere.
///
/// Ratios greater than `1.0` (e.g. an over-provisioning multiplier) need a distinct type with its
/// own bound; deliberately not invented here before a real callsite exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ppm(u32);

impl Ppm {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1_000_000);
    pub const MAX_PPM: u32 = 1_000_000;

    pub fn new(parts_per_million: u32) -> Result<Self, WireScalarError> {
        if parts_per_million > Self::MAX_PPM {
            return Err(WireScalarError::new(format!(
                "ratio {parts_per_million} ppm exceeds 1.0 ({} ppm)",
                Self::MAX_PPM
            )));
        }
        Ok(Self(parts_per_million))
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Observation/diagnostic projection only — never feed this back into a branch.
    pub fn as_ratio(self) -> f64 {
        f64::from(self.0) / f64::from(Self::MAX_PPM)
    }

    /// Convert a host-supplied ratio at the boundary (rounding to the nearest ppm). The float
    /// stops here: everything downstream compares integers.
    pub fn from_ratio(ratio: f64) -> Result<Self, WireScalarError> {
        if !ratio.is_finite() {
            return Err(WireScalarError::new("ratio must be finite"));
        }
        if !(0.0..=1.0).contains(&ratio) {
            return Err(WireScalarError::new(format!(
                "ratio {ratio} is outside [0.0, 1.0]"
            )));
        }
        Self::new((ratio * f64::from(Self::MAX_PPM)).round() as u32)
    }
}

impl Serialize for Ppm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

struct PpmVisitor;

impl Visitor<'_> for PpmVisitor {
    type Value = Ppm;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an integer number of parts-per-million in [0, 1000000]")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        u32::try_from(value)
            .map_err(|_| scalar_error::<E>(format!("{value} ppm is out of range")))
            .and_then(|value| Ppm::new(value).map_err(|err| scalar_error(err.message)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        if value < 0 {
            return Err(scalar_error(format!(
                "ppm must not be negative, got {value}"
            )));
        }
        self.visit_u64(value as u64)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "policy ratios are fixed-point parts-per-million integers, got the float {value}"
        )))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "ppm must be a JSON integer, got the string {value:?}"
        )))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(scalar_error("ppm must be a JSON integer, got null"))
    }
}

impl<'de> Deserialize<'de> for Ppm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(PpmVisitor)
    }
}

// ---------------------------------------------------------------------------------------------
// FiniteF64
// ---------------------------------------------------------------------------------------------

/// An **observation-only** float. NaN and ±Infinity never cross the boundary: they are not
/// representable in JSON, they break canonical bytes, and they turn any comparison into a
/// silent false.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct FiniteF64(f64);

impl FiniteF64 {
    pub fn new(value: f64) -> Result<Self, WireScalarError> {
        if !value.is_finite() {
            return Err(WireScalarError::new(format!(
                "observation float must be finite, got {value}"
            )));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for FiniteF64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

struct FiniteF64Visitor;

impl Visitor<'_> for FiniteF64Visitor {
    type Value = FiniteF64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a finite JSON number")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        FiniteF64::new(value).map_err(|err| scalar_error(err.message))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        FiniteF64::new(value as f64).map_err(|err| scalar_error(err.message))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        FiniteF64::new(value as f64).map_err(|err| scalar_error(err.message))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Err(scalar_error(format!(
            "observation float must be a JSON number, got the string {value:?}"
        )))
    }
}

impl<'de> Deserialize<'de> for FiniteF64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(FiniteF64Visitor)
    }
}

// ---------------------------------------------------------------------------------------------
// CanonicalBytes
// ---------------------------------------------------------------------------------------------

/// Canonical record/checkpoint bytes.
///
/// Native bindings project this to `bytes` / `Uint8Array`. The JSON projection used by
/// diagnostics and exports is **explicit**: `{"encoding":"base64","data":"…"}`. A bare string
/// would be indistinguishable from text, and a number array would double in size and invite
/// per-language element typing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CanonicalBytes(Vec<u8>);

impl CanonicalBytes {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BytesEncoding {
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalBytesProjection {
    encoding: BytesEncoding,
    data: String,
}

impl Serialize for CanonicalBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        CanonicalBytesProjection {
            encoding: BytesEncoding::Base64,
            data: base64_encode(&self.0),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CanonicalBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let projection = CanonicalBytesProjection::deserialize(deserializer)?;
        base64_decode(&projection.data)
            .map(Self)
            .map_err(|err| scalar_error(err.message))
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(BASE64_ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn base64_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strict, canonical base64: standard alphabet, mandatory padding, no whitespace, no trailing
/// bits. Anything else is a different byte string in some other decoder — which is exactly the
/// ambiguity canonical bytes exist to remove.
fn base64_decode(text: &str) -> Result<Vec<u8>, WireScalarError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(WireScalarError::new(
            "base64 payload length must be a multiple of 4 (canonical padding)",
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let is_last = index == bytes.len() / 4 - 1;
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        if pad > 0 && !is_last {
            return Err(WireScalarError::new(
                "base64 padding may only end the payload",
            ));
        }
        if pad > 2 || (pad > 0 && chunk[3] != b'=') || (pad == 2 && chunk[2] != b'=') {
            return Err(WireScalarError::new("malformed base64 padding"));
        }
        let mut triple = 0u32;
        for (position, &byte) in chunk.iter().enumerate() {
            let value = if byte == b'=' {
                0
            } else {
                base64_value(byte).ok_or_else(|| {
                    WireScalarError::new(format!("illegal base64 character {:?}", byte as char))
                })?
            };
            triple |= value << (18 - 6 * position);
        }
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// BoundedJson
// ---------------------------------------------------------------------------------------------

/// Maximum nesting depth of an opaque JSON payload carried on the wire.
///
/// Keep this aligned with the runtime's absolute envelope depth. Tool parameters are JSON Schema
/// documents, and a valid schema can naturally exceed sixteen levels before any model-authored
/// arguments exist. The envelope preflight still enforces this same finite ceiling over the whole
/// input, so widening the scalar-local guard does not create an unbounded parse path.
pub const BOUNDED_JSON_MAX_DEPTH: usize = 64;
/// Maximum number of entries in any single container of an opaque JSON payload.
pub const BOUNDED_JSON_MAX_ENTRIES: usize = 1024;

/// Opaque, host-supplied JSON (signal payloads, task metadata) with a bound on how much of it
/// the kernel is willing to carry. Unbounded free-form JSON is the one shape that can defeat
/// every downstream size budget, so the bound lives at the boundary type, not at each callsite.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoundedJson(serde_json::Value);

impl BoundedJson {
    pub fn new(value: serde_json::Value) -> Result<Self, WireScalarError> {
        validate_bounded(&value, 1)?;
        Ok(Self(value))
    }

    pub fn null() -> Self {
        Self(serde_json::Value::Null)
    }

    pub fn get(&self) -> &serde_json::Value {
        &self.0
    }

    pub fn into_value(self) -> serde_json::Value {
        self.0
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

fn validate_bounded(value: &serde_json::Value, depth: usize) -> Result<(), WireScalarError> {
    if depth > BOUNDED_JSON_MAX_DEPTH {
        return Err(WireScalarError::new(format!(
            "payload nests deeper than {BOUNDED_JSON_MAX_DEPTH}"
        )));
    }
    match value {
        serde_json::Value::Array(items) => {
            if items.len() > BOUNDED_JSON_MAX_ENTRIES {
                return Err(WireScalarError::new(format!(
                    "payload container has {} entries; the bound is {BOUNDED_JSON_MAX_ENTRIES}",
                    items.len()
                )));
            }
            items
                .iter()
                .try_for_each(|item| validate_bounded(item, depth + 1))
        }
        serde_json::Value::Object(map) => {
            if map.len() > BOUNDED_JSON_MAX_ENTRIES {
                return Err(WireScalarError::new(format!(
                    "payload container has {} entries; the bound is {BOUNDED_JSON_MAX_ENTRIES}",
                    map.len()
                )));
            }
            map.values()
                .try_for_each(|item| validate_bounded(item, depth + 1))
        }
        serde_json::Value::Number(number) => {
            // serde_json can produce a non-finite f64 from an out-of-range literal; §7.1.1 says
            // no non-finite float crosses the boundary, opaque payload or not.
            match number.as_f64() {
                Some(float) if !float.is_finite() => {
                    Err(WireScalarError::new("payload contains a non-finite number"))
                }
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

impl Serialize for BoundedJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BoundedJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::new(value).map_err(|err| scalar_error(err.message))
    }
}

// ---------------------------------------------------------------------------------------------
// branded identities
// ---------------------------------------------------------------------------------------------

fn validate_id(label: &'static str, value: &str) -> Result<(), WireScalarError> {
    if value.is_empty() {
        return Err(WireScalarError::new(format!("{label} must not be empty")));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(WireScalarError::new(format!(
            "{label} is {} bytes; the bound is {MAX_ID_BYTES}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(WireScalarError::new(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

macro_rules! wire_id {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WireScalarError> {
                let value = value.into();
                validate_id($label, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct IdVisitor;

                impl Visitor<'_> for IdVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str(concat!("a non-empty ", $label, " string"))
                    }

                    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        $name::new(value).map_err(|err| scalar_error(err.message))
                    }

                    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                        Err(scalar_error(format_args!(
                            "{} must be a branded string, got the number {}",
                            $label,
                            Unexpected::Unsigned(value)
                        )))
                    }

                    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                        Err(scalar_error(format_args!(
                            "{} must be a branded string, got the number {}",
                            $label,
                            Unexpected::Signed(value)
                        )))
                    }

                    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                        Err(scalar_error(concat!($label, " must be a branded string, got null")))
                    }
                }

                deserializer.deserialize_any(IdVisitor)
            }
        }
    };
}

wire_id!(
    /// Identity of one kernel operation. Minted once, immutable after the first accepted input.
    OperationId,
    "operation id"
);
wire_id!(
    /// Caller-suppliable idempotency key for one envelope (DEC-2). Retrying the same intent with
    /// the same `input_id` must reach the same durable record.
    InputId,
    "input id"
);
wire_id!(
    /// Kernel-minted identity of one pending effect.
    EffectId,
    "effect id"
);
wire_id!(
    /// Logical tool/provider call identity.
    CallId,
    "call id"
);
wire_id!(
    /// Logical task identity. Never a host session id.
    TaskId,
    "task id"
);
wire_id!(
    /// One execution attempt of a logical task.
    AttemptId,
    "attempt id"
);
wire_id!(
    /// Logical workflow identity.
    WorkflowId,
    "workflow id"
);
wire_id!(
    /// Node identity inside a workflow DAG.
    NodeId,
    "node id"
);
wire_id!(
    /// Logical signal identity.
    SignalId,
    "signal id"
);
wire_id!(
    /// Host delivery identity for one signal delivery attempt.
    DeliveryId,
    "delivery id"
);
wire_id!(
    /// P3 context handle identity.
    HandleId,
    "handle id"
);
wire_id!(
    /// Opaque memory access binding. Never a tenant, namespace or path.
    MemoryBindingId,
    "memory binding id"
);

// ---------------------------------------------------------------------------------------------
// Ppm const construction (appended for Task 5)
// ---------------------------------------------------------------------------------------------

impl Ppm {
    /// `const`-constructible ppm for compile-time baselines.
    ///
    /// [`Ppm::new`] returns a `Result` and therefore cannot appear in a `const` initialiser, but
    /// the kernel's default policy table *is* a compile-time constant. Values above
    /// [`Ppm::MAX_PPM`] saturate rather than panic: a default table that aborts the process at
    /// startup would turn a typo into an outage, and saturation is still a legal ratio.
    pub const fn from_ppm_const(parts_per_million: u32) -> Self {
        if parts_per_million > Self::MAX_PPM {
            Self(Self::MAX_PPM)
        } else {
            Self(parts_per_million)
        }
    }
}
