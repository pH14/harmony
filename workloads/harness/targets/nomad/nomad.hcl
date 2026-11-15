disable_update_check = true

server {
  enabled          = true
  bootstrap_expect = 3
  raft_multiplier  = 1
}

client {
  enabled = false
}
