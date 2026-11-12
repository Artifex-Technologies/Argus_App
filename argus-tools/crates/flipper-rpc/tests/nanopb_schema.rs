//! The Rust schema (proto/*.proto, the firmware's copy) against the C++ app's generated nanopb
//! code (plugins/flipperproto0/messages/*.pb.h). Every field the C++ app knows must exist in
//! the Rust schema with the same number, wire type, repetition and oneof membership, so both
//! put the same bytes on the wire for every message the C++ app can send or read. The Rust
//! schema is newer (protocol 0.25), so it may have more messages and fields; never fewer.

use std::collections::BTreeMap;
use std::path::PathBuf;

use prost::Message;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{DescriptorProto, FileDescriptorSet};

/// Messages the C++ app compiles (messages/status.pb.c) but never uses: the old `PB_Status`
/// package, removed from the protocol when ping moved to `PB_System` (tags 5 and 6 of
/// `PB.Main`, which both sides agree on). Not ported.
const DEAD_IN_CPP: &[&str] = &["PB_Status_PingRequest", "PB_Status_PingResponse"];

#[derive(Debug, Clone, PartialEq)]
struct Field {
    number: i32,
    ty: Type,
    repeated: bool,
    oneof: bool,
}

/// C struct name (`PB_Storage_ListRequest`, `PB_Region_Band`) -> field name -> field.
type Schema = BTreeMap<String, BTreeMap<String, Field>>;

fn add_message(schema: &mut Schema, c_prefix: &str, m: &DescriptorProto) {
    let c_name = format!("{c_prefix}_{}", m.name());
    let mut fields = BTreeMap::new();
    for f in &m.field {
        fields.insert(
            f.name().to_string(),
            Field {
                number: f.number(),
                ty: f.r#type(),
                repeated: f.label() == Label::Repeated,
                oneof: f.oneof_index.is_some() && !f.proto3_optional(),
            },
        );
    }
    for nested in &m.nested_type {
        add_message(schema, &c_name, nested);
    }
    schema.insert(c_name, fields);
}

fn rust_schema() -> Schema {
    let set = FileDescriptorSet::decode(flipper_rpc::proto::DESCRIPTOR_SET).unwrap();
    let mut schema = Schema::new();
    for file in &set.file {
        for m in &file.message_type {
            add_message(&mut schema, file.package(), m);
        }
    }
    schema
}

fn nanopb_type(t: &str) -> Type {
    match t {
        "UINT32" => Type::Uint32,
        "INT32" => Type::Int32,
        "UINT64" => Type::Uint64,
        "INT64" => Type::Int64,
        "SINT32" => Type::Sint32,
        "SINT64" => Type::Sint64,
        "FIXED32" => Type::Fixed32,
        "FIXED64" => Type::Fixed64,
        "SFIXED32" => Type::Sfixed32,
        "SFIXED64" => Type::Sfixed64,
        "FLOAT" => Type::Float,
        "DOUBLE" => Type::Double,
        "BOOL" => Type::Bool,
        "STRING" => Type::String,
        "BYTES" => Type::Bytes,
        "MESSAGE" | "MSG_W_CB" => Type::Message,
        "UENUM" | "ENUM" => Type::Enum,
        other => panic!("nanopb type {other} not mapped"),
    }
}

/// Parses every `#define <Msg>_FIELDLIST(X, a)` in the C++ app's generated headers.
fn cpp_schema() -> Schema {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../plugins/flipperproto0/messages");
    let mut schema = Schema::new();
    let mut headers: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    headers.retain(|p| p.to_string_lossy().ends_with(".pb.h"));
    headers.sort();
    assert_eq!(headers.len(), 8, "the C++ app has 8 generated headers in {}", dir.display());
    for h in headers {
        let text = std::fs::read_to_string(&h).unwrap();
        let mut current: Option<String> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("#define ") {
                current = rest.split_once("_FIELDLIST(X, a)").map(|(name, _)| name.to_string());
                if let Some(name) = &current {
                    schema.entry(name.clone()).or_default();
                }
                continue;
            }
            let (Some(msg), Some(body)) = (current.as_ref(), line.strip_prefix("X(a, ")) else {
                current = None;
                continue;
            };
            let body = body.trim_end_matches('\\').trim().trim_end_matches(')');
            let mut parts = body.splitn(4, ',').map(str::trim);
            let (_alloc, label, ty, rest) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            let (name, number) = rest.rsplit_once(',').unwrap();
            let name = name.trim();
            let name = if let Some(inner) = name.strip_prefix('(') { inner.split(',').nth(1).unwrap().trim() } else { name };
            let field = Field {
                number: number.trim().parse().unwrap(),
                ty: nanopb_type(ty),
                repeated: label == "REPEATED",
                oneof: label == "ONEOF",
            };
            schema.get_mut(msg).unwrap().insert(name.to_string(), field);
        }
    }
    schema
}

#[test]
fn every_cpp_field_matches_the_rust_schema() {
    let rust = rust_schema();
    let cpp = cpp_schema();
    assert_eq!(cpp.len(), 69, "C++ message count changed");
    let mut problems = Vec::new();
    let mut fields = 0;
    for (msg, cpp_fields) in &cpp {
        let Some(rust_fields) = rust.get(msg) else {
            if DEAD_IN_CPP.contains(&msg.as_str()) {
                continue;
            }
            problems.push(format!("{msg}: message missing from the Rust schema"));
            continue;
        };
        for (name, f) in cpp_fields {
            fields += 1;
            match rust_fields.get(name) {
                None => problems.push(format!("{msg}.{name}: field missing")),
                Some(r) if r != f => problems.push(format!("{msg}.{name}: C++ {f:?}, Rust {r:?}")),
                Some(_) => {}
            }
        }
    }
    assert!(problems.is_empty(), "{} mismatches:\n{}", problems.len(), problems.join("\n"));
    assert_eq!(fields, 144, "C++ field count changed");
    eprintln!("compared {fields} fields in {} messages", cpp.len());
}

#[test]
fn the_main_oneof_keeps_every_cpp_tag() {
    let rust = rust_schema();
    let cpp = cpp_schema();
    let main_cpp = &cpp["PB_Main"];
    let main_rust = &rust["PB_Main"];
    let cpp_tags: Vec<i32> = main_cpp.values().map(|f| f.number).collect();
    assert_eq!(cpp_tags.len(), 65, "command_id, status, has_next and 62 oneof members");
    for (name, f) in main_cpp {
        assert_eq!(main_rust[name].number, f.number, "{name}");
    }
}
