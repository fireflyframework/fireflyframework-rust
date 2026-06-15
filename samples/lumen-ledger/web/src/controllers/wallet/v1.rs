// Copyright 2026 Firefly Software Foundation.
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

//! Version 1 of the wallet controller.
//!
//! The controller registers itself through `#[rest_controller]`'s `inventory`
//! submission, so this binary only needs to *compile it in* (declare the
//! module) for the framework to auto-mount it — no name is referenced, hence a
//! `pub mod` rather than a re-export (which would be dead code in a binary).

pub mod wallet_controller;
