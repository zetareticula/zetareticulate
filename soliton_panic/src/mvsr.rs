// Copyright 2020 EinsteinDB Project Authors. Licensed under Apache-2.0.
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//     http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
////////////////////////////////


use std::{
    fmt,
    iter,
    str,
    string,
    vec,
    cmp,
    hash,
    mem,
    ops,
    ptr,
    slice,
    sync::{
        atomic,
        mpsc,
        Arc,
        RwLock,
        Mutex,
    },
    thread,
    time,
    convert::TryFrom,
    convert::TryInto,
    fmt::{
        Debug,
        Display,
        Formatter,
    },
    collections::{
        hash_map,
        hash_set,
        BTreeMap,
        BTreeSet,
        BinaryHeap,
        LinkedList,
        VecDeque,
    },
    error::Error as StdError,
    hash::Hash,
    cmp::{
        Ordering,
        PartialEq,
        PartialOrd,
    },
    iter::FromIterator,
    option::Option,
    option::Option::Some,
    option::Option::None,
    result::Result,
    result::Result::Ok,
    marker::PhantomData,
    str::FromStr,
    str::FromUtf8Error,
    string::String,
    string::ToString,
    vec::Vec,
    sync::atomic::{
        AtomicBool,
        AtomicUsize,
        Ordering::{
            Acquire,
            Relaxed,
            Release,
            SeqCst,
        },
    },
    sync::mpsc::{
        channel,
        Sender,
        Receiver,
        TryRecvError,
    },
    sync::Arc as SyncArc,
    sync::RwLock as SyncRwLock,
    sync::Mutex as SyncMutex,
};



use einstein_db_alexandrov_processing::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};



use einstein_db_alexandrov_processing::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};

use einstein_ml::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};

use berolina_sql::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};

use causetq::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};

use itertools::Itertools;


use super::*;


use std::sync::{
    atomic::{
        AtomicBool,
        AtomicUsize,
        Ordering::{
            Acquire,
            Relaxed,
            Release,
            SeqCst,
        },
    },
    mpsc::{
        channel,
        Sender,
        Receiver,
        TryRecvError,
    },
    Arc as SyncArc,
    RwLock as SyncRwLock,
    Mutex as SyncMutex,
};


// use protobuf::Message as Message_implements;
// use protobuf::MessageStatic as MessageStatic_implements;
// use protobuf::ProtobufEnum as ProtobufEnum_implements;
// use protobuf::ProtobufEnumStatic as ProtobufEnumStatic_implements;
// use protobuf::ProtobufError as ProtobufError_implements;
// use protobuf::ProtobufErrorStatic as ProtobufErrorStatic_implements;
// use protobuf::ProtobufResult as ProtobufResult_implements;


use protobuf::{
    Message as Message_implements,
    MessageStatic as MessageStatic_implements,
    ProtobufEnum as ProtobufEnum_implements,
    ProtobufEnumStatic as ProtobufEnumStatic_implements,
    ProtobufError as ProtobufError_implements,
    ProtobufErrorStatic as ProtobufErrorStatic_implements,
    ProtobufResult as ProtobufResult_implements,
};


use einstein_db_alexandrov_processing::{
    index::{
        Index,
        IndexIterator,
        IndexIteratorOptions,
        IndexIteratorOptionsBuilder,
    },
};






