// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/history"
	"harmony.local/harness/kit/runner"
)

func main() {
	name := flag.String("family", "log", "family checker to run: log or register")
	emit := flag.Bool("emit", false, "also write SDK assertion records to $ANTITHESIS_OUTPUT_DIR")
	flag.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: harness-check [-family log|register] [-emit] JOURNAL_DIR")
		flag.PrintDefaults()
	}
	flag.Parse()
	if flag.NArg() != 1 {
		flag.Usage()
		os.Exit(2)
	}
	fam, err := family.ByName(*name)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	h, err := history.Load(flag.Arg(0))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	report := fam.Check(h)
	if *emit {
		runner.Declare(fam)
		runner.Emit(fam, report)
	}
	out, err := json.Marshal(map[string]any{
		"files":      h.Files,
		"ops":        len(h.Ops),
		"malformed":  h.Malformed,
		"decided":    report.Decided,
		"facts":      report.Facts,
		"violations": report.Violations,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println(string(out))
	if len(report.Violations) > 0 {
		os.Exit(3)
	}
}
