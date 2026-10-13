// Copyright 2026 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! DTLN-aec-128, both stages, as published by its authors (MIT, Copyright (c)
//! 2020 Nils L. Westhausen; `weights/LICENSE`), float32, embedded in the
//! library. The test `weights_are_the_published_files` checks the hashes.

/// Where the files come from.
pub const SOURCE: &str = "https://github.com/breizhn/DTLN-aec/tree/9d24e128b4f409db18227b8babb343016625921f/pretrained_models";

/// `dtln_aec_128_1.tflite`: stage 1 (frequency-domain mask), 1,996,600 bytes.
pub const STAGE1: &[u8] = include_bytes!("../weights/dtln_aec_128_1.tflite");
pub const STAGE1_SHA256: &str = "8d241b3a732af8ca140b2e30043e56a6c3c7800c46e22c26f4eea2f70974ad1e";

/// `dtln_aec_128_2.tflite`: stage 2 (learned-basis block), 5,277,012 bytes.
pub const STAGE2: &[u8] = include_bytes!("../weights/dtln_aec_128_2.tflite");
pub const STAGE2_SHA256: &str = "350bb01a1152ae3cabe09fe5e868ef2f7d8b988a9f22aae44f140195f6493126";

/// The repository's MIT license, which covers the weights.
pub const LICENSE: &str = include_str!("../weights/LICENSE");
