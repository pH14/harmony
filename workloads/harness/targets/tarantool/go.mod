module harmony.local/harness/targets/tarantool

go 1.26.0

require (
	github.com/tarantool/go-iproto v1.1.0
	github.com/tarantool/go-tarantool/v3 v3.0.2
	harmony.local/harness/kit v0.0.0
)

require (
	github.com/anishathalye/porcupine v1.3.1 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/tarantool/go-option v1.1.0 // indirect
	github.com/vmihailenco/msgpack/v5 v5.4.1 // indirect
	github.com/vmihailenco/tagparser/v2 v2.0.0 // indirect
	golang.org/x/sys v0.38.0 // indirect
)

replace harmony.local/harness/kit => ../../kit
