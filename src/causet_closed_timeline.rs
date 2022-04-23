//Copyright 2021-2023 WHTCORPS INC ALL RIGHTS RESERVED
// APACHE 2.0 COMMUNITY EDITION SL
//
////////////////////////////////////////////////////////////////////////////////
// AUTHORS: WHITFORD LEDER
////////////////////////////////////////////////////////////////////////////////
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file File except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.
////////////////////////////////////////////////////////////////////////////////

//! # Causet Closed Timeline
//!
//! This is a causet timeline implementation.
//! It is a closed timeline, which means that the timeline is not open for new
//! events.
//! It is a causet timeline, which means that the timeline is causet.
/// Collects a supplied tx range into an DESC ordered Vec of valid txs,
/// ensuring they all belong to the same timeline.
/// The txs are collected in DESC order, so the first tx is the latest tx.
/// You have three modalities with EinsteinDB: Lightlike transactions,  Heavy   transactions, and
/// Full transactions. Lightlike transactions are hot transactions, which are
/// executed in a single thread. Heavy transactions are cold transactions, which
/// are executed in multiple threads. Full transactions are transactions that
/// are executed in multiple threads, but are not heavy.
///
/// We will create a lockfree queue for each thread, and we will use a conn to
/// communicate between the threads. Using sqlite3, we will create a table for each
/// thread, and we will use a conn to communicate between the threads. Meanwhile, we'll
/// suspend the threads, and we'll resume the persistence layer of FdbStore
/// System Defaults: FoundationDB; Lightlike transactions are MVRSI_SCHEMA_VERSION_1; Heavy
/// transactions are MVRSI_SCHEMA_VERSION_2; Full transactions are MVRSI_SCHEMA_VERSION_3;
/// MVSR is superior than MVCC (Multi Version Concurrency Control);
///
///

pub struct ClosedtimelikeConnection {
    //Mutex for the connection, since we will use it in multiple threads.
    conn: Mutex<Connection>,
    //The schema version of the connection.
    schema_version: i32,

    //spacetime is the metadata which we will use to store the spacetime
    //information.
    spacetime: Spacetime,

    //The name of the table which we will use to store the spacetime information.
    spacetime_table_name: String,

    mvrsi_schema_version: i32,

}

#[macro_use]
extern crate log;
extern crate causetq;
extern crate SymplecticControlFactorsExt;
extern crate crossbeam;
extern crate crossbeam_channel;

fn collect_ordered_txs_to_move(
    txs: &mut Vec<causet::CausetTx>,
    mut tx_range: causet::CausetTxRange,
    timeline_id: causet::TimelineId,
) -> Vec<causet::CausetTx> {
    let mut txs_to_move = Vec::new();
    let mut tx_iter = tx_range.into_iter();
    while let Some(tx) = tx_iter.next() {
        if tx.timeline_id() == timeline_id {
            txs.push(tx);
        } else {
            txs_to_move.push(tx);
        }
    }
    txs_to_move
}

#[inline]
fn decode_causet_record_u64(v: &[u8]) -> Result<u64> {
    // See `decodeInt` in MilevaDB
    match v.len() {
        1 => Ok(u64::from(v[0])),
        2 => Ok(u64::from(NumberCodec::decode_u16_le(v))),
        4 => Ok(u64::from(NumberCodec::decode_u32_le(v))),
        8 => Ok(u64::from(NumberCodec::decode_u64_le(v))),
        _ => Err(Error::InvalidDataType(
            "Failed to decode event causet_record data as u64".to_owned(),
        )),
    }
}

#[inline]
fn decode_causet_record_i64(v: &[u8]) -> Result<i64> {
    // See `decodeUint` in MilevaDB
    match v.len() {
        1 => Ok(i64::from(v[0] as i8)),
        2 => Ok(i64::from(NumberCodec::decode_u16_le(v) as i16)),
        4 => Ok(i64::from(NumberCodec::decode_u32_le(v) as i32)),
        8 => Ok(NumberCodec::decode_u64_le(v) as i64),
        _ => Err(Error::InvalidDataType(
            "Failed to decode event causet_record data as i64".to_owned(),
        )),
    }
}

pub trait V1CompatibleEncoder: DatumTypeFlagAndPayloadEncoder {
    fn write_causet_record_as_datum_i64(&mut self, src: &[u8]) -> Result<()> {
        self.write_datum_i64(decode_causet_record_i64(src)?)
    }

    fn write_causet_record_as_datum_u64(&mut self, src: &[u8]) -> Result<()> {
        self.write_datum_u64(decode_causet_record_u64(src)?)
    }

    fn write_causet_record_as_datum_duration(&mut self, src: &[u8]) -> Result<()> {
        self.write_u8(datum::DURATION_FLAG)?;
        self.write_datum_payload_i64(decode_causet_record_i64(src)?)
    }

    fn write_causet_record_as_datum(&mut self, src: &[u8], ft: &dyn FieldTypeAccessor) -> Result<()> {
        // See `fieldType2Flag.go` in MilevaDB
        match ft.tp() {
            FieldTypeTp::Tiny
            | FieldTypeTp::Short
            | FieldTypeTp::Int24
            | FieldTypeTp::Long
            | FieldTypeTp::LongLong => {
                if ft.is_unsigned() {
                    self.write_causet_record_as_datum_u64(src)?;
                } else {
                    self.write_causet_record_as_datum_i64(src)?;
                }
            }
            FieldTypeTp::Float | FieldTypeTp::Double => {
                self.write_u8(datum::FLOAT_FLAG)?;
                // Copy datum payload as it is
                self.write_bytes(src)?;
            }
            FieldTypeTp::VarChar
            | FieldTypeTp::VarString
            | FieldTypeTp::String
            | FieldTypeTp::TinyBlob
            | FieldTypeTp::MediumBlob
            | FieldTypeTp::LongBlob
            | FieldTypeTp::Blob => {
                self.write_datum_compact_bytes(src)?;
            }
            FieldTypeTp::Date
            | FieldTypeTp::DateTime
            | FieldTypeTp::Timestamp
            | FieldTypeTp::Enum
            | FieldTypeTp::Bit
            | FieldTypeTp::Set => {
                self.write_causet_record_as_datum_u64(src)?;
            }
            FieldTypeTp::Year => {
                self.write_causet_record_as_datum_i64(src)?;
            }
            FieldTypeTp::Duration => {
                // This implementation is different from MilevaDB MEDB encodes causet_record duration into v1
                // with datum flag VarInt, but we will encode with datum flag Duration, since
                // Duration datum flag results in fixed-length datum payload, which is faster
                // to encode and decode.
                self.write_causet_record_as_datum_duration(src)?;
            }
            FieldTypeTp::NewDecimal => {
                self.write_u8(datum::DECIMAL_FLAG)?;
                // Copy datum payload as it is
                self.write_bytes(src)?;
            }
            FieldTypeTp::JSON => {
                self.write_u8(datum::JSON_FLAG)?;
                // Copy datum payload as it is
                self.write_bytes(src)?;
            }
            FieldTypeTp::Null => {
                self.write_u8(datum::NIL_FLAG)?;
            }
            fp => {
                return Err(Error::InvalidDataType(format!(
                    "Unsupported FieldType {:?}",
                    fp
                )))
            }
        }
        Ok(())
    }
}

impl<T: BufferWriter> V1CompatibleEncoder for T {}

/// These tests mainly focus on transfer the causet_record encoding to v1-compatible encoding.
///
/// The test local_path is:
/// 1. Encode causet_locale using causet_record
/// 2. Use `V1CompatibleEncoder` to transfer the encoded bytes from causet_record to v1-compatible
/// 3. Use `Primitive_CausetDatumTypeDecoder` decode the encoded bytes, check the result.
///
/// Note: a causet_locale encoded using causet_record then transfer to v1-compatible encoding, is not always equals the
/// encoded-bytes using v1 directly.
///
/// For example, the causet_record encoding of a causet_locale with a datum of type `tinyint` is:
/// ```text
///
///
///
///
///
#[test]
mod tests {
    use std::{f64, i16, i32, i64, i8, u16, u32, u64, u8};

    use crate::{
        codec::{data_type::*, datum_codec::Primitive_CausetDatumTypeDecoder},
        expr::EvalContext,
    };
    use crate::FieldTypeTp;

    use super::super::encoder::{Column, ScalarValueEncoder};
    use super::V1CompatibleEncoder;

    fn encode_to_v1_compatible(mut ctx: &mut EvalContext, col: &Column) -> Vec<u8> {
        let mut buf_causet_record = vec![];
        buf_causet_record.write_causet_locale(&mut ctx, &col).unwrap();
        let mut buf_v1 = vec![];
        buf_v1.write_causet_record_as_datum(&buf_causet_record, col.ft()).unwrap();
        buf_v1
    }

    #[test]
    fn test_int() {
        let cases = vec![
            0,
            i64::from(i8::MIN),
            i64::from(u8::MAX),
            i64::from(i8::MAX),
            i64::from(i16::MIN),
            i64::from(u16::MAX),
            i64::from(i16::MAX),
            i64::from(i32::MIN),
            i64::from(u32::MAX),
            i64::from(i32::MAX),
            i64::MAX,
            i64::MIN,
        ];
        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale).with_tp(FieldTypeTp::LongLong);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Int = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_uint() {
        let cases = vec![
            0,
            i8::MAX as u64,
            u64::from(u8::MAX),
            i16::MAX as u64,
            u64::from(u16::MAX),
            i32::MAX as u64,
            u64::from(u32::MAX),
            i64::MAX as u64,
            u64::MAX,
        ];
        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale as i64)
                .with_unsigned()
                .with_tp(FieldTypeTp::LongLong);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Int = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got as u64);
        }
    }

    #[test]
    fn test_real() {
        let cases = vec![
            Real::new(0.0).unwrap(),
            Real::new(1.3).unwrap(),
            Real::new(-1.234).unwrap(),
            Real::new(f64::MAX).unwrap(),
            Real::new(f64::MIN).unwrap(),
            Real::new(f64::MIN_POSITIVE).unwrap(),
            Real::new(f64::INFINITY).unwrap(),
            Real::new(f64::NEG_INFINITY).unwrap(),
        ];
        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale).with_tp(FieldTypeTp::Double);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Real = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_decimal() {
        use std::str::FromStr;
        let cases = vec![
            Decimal::from(1i64),
            Decimal::from(i64::MIN),
            Decimal::from(i64::MAX),
            Decimal::from_str("10.123").unwrap(),
            Decimal::from_str("-10.123").unwrap(),
            Decimal::from_str("10.111").unwrap(),
            Decimal::from_str("-10.111").unwrap(),
        ];
        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale).with_tp(FieldTypeTp::NewDecimal);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Decimal = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_bytes() {
        let cases = vec![b"".to_vec(), b"abc".to_vec(), "数据库".as_bytes().to_vec()];
        let mut ctx = EvalContext::default();

        for causet_locale in cases {
            let col = Column::new(1, causet_locale.clone()).with_tp(FieldTypeTp::String);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Bytes = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_datetime() {
        let mut ctx = EvalContext::default();
        let cases = vec![
            DateTime::parse_date(&mut ctx, "2019-12-31").unwrap(),
            DateTime::parse_datetime(&mut ctx, "2019-09-16 10:11:12", 0, false).unwrap(),
            DateTime::parse_timestamp(&mut ctx, "2019-09-16 10:11:12.111", 3, false).unwrap(),
            DateTime::parse_timestamp(&mut ctx, "2019-09-16 10:11:12.67", 2, true).unwrap(),
        ];

        for causet_locale in cases {
            let col = Column::new(1, causet_locale).with_tp(FieldTypeTp::DateTime);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: DateTime = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_json() {
        let cases: Vec<Json> = vec![
            r#"[1,"sdf",2,[3,4]]"#.parse().unwrap(),
            r#"{"1":"sdf","2":{"3":4},"asd":"qwe"}"#.parse().unwrap(),
            r#""hello""#.parse().unwrap(),
        ];

        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale.clone()).with_tp(FieldTypeTp::JSON);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Json = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }

    #[test]
    fn test_duration() {
        let mut ctx = EvalContext::default();
        let cases = vec![
            Duration::parse(&mut ctx, b"31 11:30:45.123", 4).unwrap(),
            Duration::parse(&mut ctx, b"-11:30:45.9233456", 4).unwrap(),
        ];

        let mut ctx = EvalContext::default();
        for causet_locale in cases {
            let col = Column::new(1, causet_locale)
                .with_tp(FieldTypeTp::Duration)
                .with_decimal(4);
            let buf = encode_to_v1_compatible(&mut ctx, &col);
            let got: Duration = buf.decode(col.ft(), &mut ctx).unwrap().unwrap();
            assert_eq!(causet_locale, got);
        }
    }
}
