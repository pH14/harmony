;; SPDX-License-Identifier: AGPL-3.0-or-later
(module
 (import "harmony_v1" "request" (func $request (param i32 i32 i32 i32 i32) (result i32)))
 (memory (export "memory") 1 1)
 (type $t (func (result f64)))
 (table 1 1 funcref) (elem (i32.const 0) $nested)
 (global $count (mut i32) (i32.const 0))
 (data (i32.const 0) "\09\00\07\00\00\00\00\00\00\00\aa\bb\cc\dd")
 (data (i32.const 64) "\04\00\00\00")
 (data (i32.const 128) "\00\00\00\04")
 (data (i32.const 160) "\02\00\00\04\01\00\00\00\01\00\00\00\50\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00")
 (func $nested (result f64) (local $held f64)
  f64.const 0x1.23456789abcdep+12 local.set $held
  loop global.get $count i32.const 1 i32.add global.set $count
   i32.const 320 global.get $count i32.store
   global.get $count i32.const 20000 i32.lt_u br_if 0 end
  i32.const 393219 i32.const 0 i32.const 14 i32.const 32 i32.const 5 call $request drop
  local.get $held)
 (func (export "run")
  i32.const 256 f32.const nan:0x123 f32.const 1 f32.add f32.store
  i32.const 264 f64.const inf f64.const 0 f64.mul f64.store
  i32.const 272 f32.const -0 f32.const 1 f32.div f32.store
  i32.const 280 f64.const 0 f64.const -0 f64.min f64.store
  i32.const 288 f32.const 0x1p-126 f32.const 0.5 f32.mul f32.store
  i32.const 296 i32.const 0 call_indirect (type $t) f64.store
  i32.const 304 f32.const nan:0x123 f64.promote_f32 f64.store
  i32.const 312 f64.const nan:0x123 f32.demote_f64 f32.store
  i32.const 131073 i32.const 64 i32.const 4 i32.const 80 i32.const 4 call $request drop
  i32.const 524289 i32.const 64 i32.const 4 i32.const 84 i32.const 4 call $request drop
  i32.const 262145 i32.const 160 i32.const 28 i32.const 0 i32.const 0 call $request drop
  i32.const 262145 i32.const 128 i32.const 4 i32.const 0 i32.const 0 call $request drop
  loop br 0 end))
