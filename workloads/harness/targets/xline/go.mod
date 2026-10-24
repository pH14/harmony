module harmony.local/harness/targets/xline

go 1.26.0

require (
	go.etcd.io/etcd/api/v3 v3.7.3
	google.golang.org/grpc v1.84.0
	harmony.local/harness/kit v0.0.0
)

require (
	github.com/anishathalye/porcupine v1.3.1 // indirect
	github.com/golang/protobuf v1.5.4 // indirect
	github.com/grpc-ecosystem/grpc-gateway/v2 v2.29.0 // indirect
	golang.org/x/net v0.61.0 // indirect
	golang.org/x/sys v0.49.0 // indirect
	golang.org/x/text v0.43.0 // indirect
	google.golang.org/genproto/googleapis/api v0.0.0-20260803160001-6ac0973c030d // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20260803160001-6ac0973c030d // indirect
	google.golang.org/protobuf v1.36.11 // indirect
)

replace harmony.local/harness/kit => ../../kit
