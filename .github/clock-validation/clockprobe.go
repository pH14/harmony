// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"fmt"
	"os"
	"time"
)

func main() {
	name := "probe"
	if len(os.Args) > 1 {
		name = os.Args[1]
	}
	start := time.Now()
	for {
		now := time.Now()
		fmt.Printf("%s wall=%s unix=%d monotonic_ms=%d\n", name,
			now.UTC().Format(time.RFC3339Nano), now.Unix(), now.Sub(start).Milliseconds())
		time.Sleep(time.Second)
	}
}
