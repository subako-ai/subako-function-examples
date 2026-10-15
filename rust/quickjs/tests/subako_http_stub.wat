;; Stands in for the worker's `subako_http` import, which wasmtime lacks, so
;; the module instantiates outside Subako. Every request fails as an ABI
;; misuse, which the SDK reports as an `abi` error.
(module
  (func (export "request") (param i32 i32) (result i32) i32.const -1)
  (func (export "read_response") (param i32 i32) (result i32) i32.const -2))
