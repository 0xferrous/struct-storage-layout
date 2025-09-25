use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader},
    str::FromStr,
};

use eyre::OptionExt;
use regex::Regex;

#[derive(Debug, Clone)]
enum SolType {
    Uint(u16),
    Int(u16),
    Address,
    Bool,
    Bytes(u8),
    BytesArbitrary,
    Custom(SolStruct),
    Custom2(String),
    #[allow(dead_code)]
    Mapping(Box<SolType>, Box<SolType>),
    #[allow(dead_code)]
    Array(Box<SolType>),
    FixedArray(Box<SolType>, u64),
}

fn snap_to_upper_256(size: u64) -> u64 {
    let over = size % 256;
    let size = if over == 0 { size } else { size + 256 - over };
    assert!(size.is_multiple_of(256));

    size
}

// Storage layout rules: https://docs.soliditylang.org/en/latest/internals/layout_in_storage.html
//
// - The first item in a storage slot is stored lower-order aligned.
// - Value types use only as many bytes as are necessary to store them.
// - If a value type does not fit the remaining part of a storage slot, it is stored in the next storage slot.
// - Structs and array data always start a new slot and their items are packed tightly according to these rules.
// - Items following struct or array data always start a new storage slot.
impl SolType {
    fn size(&self, all_structs: &BTreeMap<String, SolStruct>) -> eyre::Result<u64> {
        Ok(match self {
            Self::Uint(size) => (*size).into(),
            Self::Int(size) => (*size).into(),
            Self::Address => (20u32 * 8).into(),
            Self::Bool => 1,
            Self::Bytes(size) => *size as u64 * 8,
            Self::BytesArbitrary => 256,
            Self::Custom(sol_struct) => {
                let mut size = 0;
                let mut current_word_bits_allocated = 0;

                fn update_state(
                    typ: &SolType,
                    current_word_bits_allocated: &mut u64,
                    size: &mut u64,
                    all_structs: &BTreeMap<String, SolStruct>,
                ) -> eyre::Result<()> {
                    let remainder_bits = 256 - *current_word_bits_allocated;

                    match typ {
                        // Value types use up only as many bytes as necessary if available, or
                        // start on new slot if not enough space.
                        SolType::Uint(_)
                        | SolType::Int(_)
                        | SolType::Address
                        | SolType::Bool
                        | SolType::Bytes(_) => {
                            let bits_needed = typ.size(all_structs)?;
                            if bits_needed <= remainder_bits {
                                *current_word_bits_allocated += bits_needed;
                                *size += bits_needed;
                            } else {
                                // move to next slot
                                *current_word_bits_allocated = 0;
                                *size += remainder_bits;
                                // allocate bits in next slot
                                *size += bits_needed;
                                *current_word_bits_allocated += bits_needed;
                            }
                        }
                        // Fixed array types are inlined
                        SolType::FixedArray(sol_type, len) => {
                            // move to next slot
                            *current_word_bits_allocated = 0;
                            *size = snap_to_upper_256(*size);

                            for _ in 0..*len {
                                update_state(
                                    sol_type,
                                    current_word_bits_allocated,
                                    size,
                                    all_structs,
                                )?;
                            }
                        }
                        // Mapping, Dynamic size array, arbitrary bytes, all take up the next full
                        // slot.
                        SolType::Mapping(_, _) | SolType::Array(_) | SolType::BytesArbitrary => {
                            *current_word_bits_allocated = 0;
                            *size = snap_to_upper_256(*size);
                            *size += 256;
                        }
                        // Structs are packed tightly according to the rules above.
                        // And they always start on a new slot.
                        // Items following structs always start on a new slot
                        SolType::Custom(_) => {
                            *current_word_bits_allocated = 0;
                            *size = snap_to_upper_256(*size);
                            *size += typ.size(all_structs)?;
                            *size = snap_to_upper_256(*size);
                        }
                        SolType::Custom2(st_name) => {
                            let typ = SolType::Custom(
                                all_structs
                                    .get(st_name)
                                    .ok_or_eyre(format!("struct not found: {st_name}"))?
                                    .clone(),
                            );
                            update_state(&typ, current_word_bits_allocated, size, all_structs)?;
                        }
                    }

                    Ok(())
                }

                for (_, typ) in &sol_struct.fields {
                    update_state(
                        typ,
                        &mut current_word_bits_allocated,
                        &mut size,
                        all_structs,
                    )?;
                }

                size
            }
            Self::Custom2(st_name) => {
                let typ = Self::Custom(
                    all_structs
                        .get(st_name)
                        .ok_or_eyre(format!("unknown struct: {st_name}"))?
                        .clone(),
                );
                typ.size(all_structs)?
            }
            Self::Mapping(_, _) => 256,
            Self::Array(_) => 256,
            Self::FixedArray(sol_type, len) => {
                let size = sol_type.size(all_structs)?;
                let remainder = 256 - (size % 256);
                let size = size + remainder;
                assert!(size % 256 == 0);

                size * len
            }
        })
    }
}

const MAPPING_REGEX: &str =
    r"\s*mapping\s*\(\s*(?<key_type>\w+)\s*=>\s*(?<value_type>\w+(?:\[\d*\])?)\s*\)";
const FIXED_ARRAY_REGEX: &str = r"\s*(?<type>\w+)\s*\[\s*(?<size>\d+)\s*\]\s*";

impl FromStr for SolType {
    type Err = eyre::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "uint" => Self::Uint(256),
            "int" => Self::Int(256),
            "address" => Self::Address,
            "bool" => Self::Bool,
            "bytes" => Self::BytesArbitrary,
            "bytes1" | "bytes2" | "bytes3" | "bytes4" | "bytes5" | "bytes6" | "bytes7"
            | "bytes8" | "bytes9" | "bytes10" | "bytes11" | "bytes12" | "bytes13" | "bytes14"
            | "bytes15" | "bytes16" | "bytes17" | "bytes18" | "bytes19" | "bytes20" | "bytes21"
            | "bytes22" | "bytes23" | "bytes24" | "bytes25" | "bytes26" | "bytes27" | "bytes28"
            | "bytes29" | "bytes30" | "bytes31" | "bytes32" => {
                Self::Bytes(s.replace("bytes", "").parse()?)
            }
            "uint8" | "uint16" | "uint24" | "uint32" | "uint40" | "uint48" | "uint56"
            | "uint64" | "uint72" | "uint80" | "uint88" | "uint96" | "uint104" | "uint112"
            | "uint120" | "uint128" | "uint136" | "uint144" | "uint152" | "uint160" | "uint168"
            | "uint176" | "uint184" | "uint192" | "uint200" | "uint208" | "uint216" | "uint224"
            | "uint232" | "uint240" | "uint248" | "uint256" => {
                Self::Uint(s.replace("uint", "").parse()?)
            }
            "int8" | "int16" | "int24" | "int32" | "int40" | "int48" | "int56" | "int64"
            | "int72" | "int80" | "int88" | "int96" | "int104" | "int112" | "int120" | "int128"
            | "int136" | "int144" | "int152" | "int160" | "int168" | "int176" | "int184"
            | "int192" | "int200" | "int208" | "int216" | "int224" | "int232" | "int240"
            | "int248" | "int256" => Self::Int(s.replace("int", "").parse()?),
            s if s.starts_with("mapping") => {
                let captures = Regex::new(MAPPING_REGEX)
                    .map_err(|e| eyre::eyre!("mapping regex instantiation error: {e}"))?
                    .captures(s)
                    .ok_or_eyre(format!("mapping didnt match: {s}"))?;
                let key_type = &captures["key_type"];
                let value_type = &captures["value_type"];

                Self::Mapping(
                    Box::new(
                        (key_type.parse::<Self>())
                            .map_err(|e| eyre::eyre!("error parsing {key_type} {e}"))?,
                    ),
                    Box::new(
                        (value_type.parse::<Self>())
                            .map_err(|e| eyre::eyre!("error parsing {value_type} {e}"))?,
                    ),
                )
            }
            s if s.ends_with("[]") => {
                let inner_type = s.replace("[]", "").parse::<Self>()?;
                Self::Array(Box::new(inner_type))
            }
            s if s.contains("[") && s.contains("]") => {
                let captures = Regex::new(FIXED_ARRAY_REGEX)
                    .map_err(|e| eyre::eyre!("fixed array regex instantiation error: {e}"))?
                    .captures(s)
                    .ok_or_eyre(format!("fixed array didnt match: {s}"))?;
                let value_type = &captures["type"];
                let size = &captures["size"];
                let size = size
                    .parse::<u64>()
                    .map_err(|e| eyre::eyre!("error parsing {size} {e}"))?;
                Self::FixedArray(
                    Box::new(
                        value_type
                            .parse::<Self>()
                            .map_err(|e| eyre::eyre!("error parsing {value_type} {e}"))?,
                    ),
                    size,
                )
            }
            _ => Self::Custom2(s.to_string()),
        })
    }
}

#[derive(Debug, Clone)]
struct SolStruct {
    name: String,
    fields: Vec<(String, SolType)>,
    _inner: String,
}

fn chunk_structs(src: &str) -> eyre::Result<Vec<String>> {
    let mut structs = vec![];

    let mut curr_struct = vec![];
    for line in src.lines() {
        if line.trim().is_empty() {
            continue;
        }

        curr_struct.push(line.to_string());
        if line.contains("}") {
            structs.push(curr_struct.join("\n"));
            curr_struct = vec![];
        }
    }

    Ok(structs)
}

fn parse_struct(src: &str) -> eyre::Result<SolStruct> {
    let mut struct_name = "";
    let mut fields = vec![];

    for line in src.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }

        if line.contains("struct") {
            let st_name = line
                .split_once("struct")
                .expect("struct not found")
                .1
                .trim()
                .split_once("{")
                .expect("{  not found")
                .0
                .trim();
            struct_name = st_name;
        } else if let Some((bf, _af)) = line.split_once(";") {
            let splits = bf.split_whitespace().collect::<Vec<_>>();
            if splits.len() > 1 {
                let field = splits.iter().last().unwrap().to_string();
                let typ = splits[..splits.len() - 1].join(" ");

                fields.push((field.replace(";", ""), typ.parse()?))
            }
        } else if line.trim() == "}" {
            // do nothing
        } else {
            eyre::bail!("invalid line: {line}");
        }
    }

    Ok(SolStruct {
        name: struct_name.to_string(),
        fields,
        _inner: src.to_string(),
    })
}

fn main() -> eyre::Result<()> {
    println!("reading from stdin..");
    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut content = String::new();
    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                content.push_str(&line);
                content.push('\n'); // Add newline back as `lines()` strips it
            }
            Err(error) => {
                eprintln!("Error reading line: {}", error);
                break;
            }
        }
    }

    println!("\n--- Content read from stdin ---");
    println!("{}", content); // Use print! instead of println! to avoid extra newline
    println!("--- End of stdin ---");

    let chunked = chunk_structs(&content)?;
    // for (i, st) in chunked.iter().enumerate() {
    //     println!("{i}: {st}");
    // }

    let structs = chunked
        .into_iter()
        .map(|st| parse_struct(&st).map(|st| (st.name.clone(), st)))
        .collect::<eyre::Result<BTreeMap<String, SolStruct>>>()?;

    for (name, st) in structs.iter().rev() {
        println!("{name}:\n-------");
        for (name, typ) in &st.fields {
            println!("{name}: {:?}", typ);
        }

        let size = SolType::Custom(st.clone()).size(&structs)?;
        let bytes = snap_to_upper_256(size) / 256;
        println!("{name}: {bytes} [{size}]");
    }

    Ok(())
}
