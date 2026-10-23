module harmony.local/harness/targets/zookeeper

go 1.26.0

require (
    harmony.local/harness/kit v0.0.0
    github.com/go-zookeeper/zk v1.0.4
)

require (
    github.com/anishathalye/porcupine v1.3.1 // indirect
    golang.org/x/sys v0.38.0 // indirect
)

replace harmony.local/harness/kit => ../../kit
