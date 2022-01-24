// Copyright 2022 Whtcorps Inc and EinstAI Inc
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

extern crate einsteinml;
extern crate core_traits;
extern crate einstai_einsteineinsteindb;
extern crate ordered_float;
extern crate rusqlite;

use ordered_float::OrderedFloat;

use einsteinml::symbols;

use core_traits::{
    TypedValue,
    ValueType,
};
use einstai_einsteineinsteindb::einsteineinsteindb::TypedSQLValue;

// It's not possible to test to_sql_value_pair since rusqlite::ToSqlOutput doesn't implement
// PartialEq.
#[test]
fn test_from_sql_value_pair() {
    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Integer(1234), 0).unwrap(), TypedValue::Ref(1234));

    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Integer(0), 1).unwrap(), TypedValue::Boolean(false));
    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Integer(1), 1).unwrap(), TypedValue::Boolean(true));

    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Integer(0), 5).unwrap(), TypedValue::Long(0));
    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Integer(1234), 5).unwrap(), TypedValue::Long(1234));

    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Real(0.0), 5).unwrap(), TypedValue::Double(OrderedFloat(0.0)));
    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Real(0.5), 5).unwrap(), TypedValue::Double(OrderedFloat(0.5)));

    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Text(":einsteineinsteindb/keyword".into()), 10).unwrap(), TypedValue::typed_string(":einsteineinsteindb/keyword"));
    assert_eq!(TypedValue::from_sql_value_pair(rusqlite::types::Value::Text(":einsteineinsteindb/keyword".into()), 13).unwrap(), TypedValue::typed_ns_keyword("einsteineinsteindb", "keyword"));
}

#[test]
fn test_to_einsteinml_value_pair() {
    assert_eq!(TypedValue::Ref(1234).to_einsteinml_value_pair(), (einsteinml::Value::Integer(1234), ValueType::Ref));

    assert_eq!(TypedValue::Boolean(false).to_einsteinml_value_pair(), (einsteinml::Value::Boolean(false), ValueType::Boolean));
    assert_eq!(TypedValue::Boolean(true).to_einsteinml_value_pair(), (einsteinml::Value::Boolean(true), ValueType::Boolean));

    assert_eq!(TypedValue::Long(0).to_einsteinml_value_pair(), (einsteinml::Value::Integer(0), ValueType::Long));
    assert_eq!(TypedValue::Long(1234).to_einsteinml_value_pair(), (einsteinml::Value::Integer(1234), ValueType::Long));

    assert_eq!(TypedValue::Double(OrderedFloat(0.0)).to_einsteinml_value_pair(), (einsteinml::Value::Float(OrderedFloat(0.0)), ValueType::Double));
    assert_eq!(TypedValue::Double(OrderedFloat(0.5)).to_einsteinml_value_pair(), (einsteinml::Value::Float(OrderedFloat(0.5)), ValueType::Double));

    assert_eq!(TypedValue::typed_string(":einsteineinsteindb/keyword").to_einsteinml_value_pair(), (einsteinml::Value::Text(":einsteineinsteindb/keyword".into()), ValueType::String));
    assert_eq!(TypedValue::typed_ns_keyword("einsteineinsteindb", "keyword").to_einsteinml_value_pair(), (einsteinml::Value::Keyword(symbols::Keyword::namespaced("einsteineinsteindb", "keyword")), ValueType::Keyword));
}
