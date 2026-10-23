// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strconv"
	"strings"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

// The script runs inside one Raft command, so the comparison and the write
// apply together on every member.
const casScript = `var v = sdb.call("get", KEYS[0]); if (v === ARGV[0]) { sdb.call("set", KEYS[0], ARGV[1]); return "applied"; } return "differs";`

type adapter struct {
	servers []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return &client{servers: a.servers}, nil
}

type client struct {
	servers []string
}

func (c *client) Close() {}

type replyError string

func (e replyError) Error() string { return string(e) }

// These replies come from a server that did not append the command to its
// Raft log.
func notApplied(err error) bool {
	var reply replyError
	if !errors.As(err, &reply) {
		return false
	}
	text := string(reply)
	return strings.HasPrefix(text, "TRY ") || text == "ERR leader not known" ||
		text == "ERR raft not ready" || strings.HasPrefix(text, "ERR unknown command")
}

// Each operation goes to one server drawn from the SDK stream. A follower
// answers "TRY <leader>", and the client follows that redirect once.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	base := c.servers[sdk.Below(uint64(len(c.servers)))]
	switch op.F {
	case family.RegisterWrite:
		_, err := c.call(ctx, base, "SET", op.K, op.V.(string))
		return nil, classify(err)
	case family.RegisterCAS:
		pair := op.V.([]string)
		reply, err := c.call(ctx, base, "EVAL", casScript, "1", op.K, pair[0], pair[1])
		if err != nil {
			return nil, classify(err)
		}
		switch reply {
		case "applied":
			return nil, nil
		case "differs":
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return nil, fmt.Errorf("cas: unexpected reply %v", reply)
	case family.RegisterReadF:
		reply, err := c.call(ctx, base, "GET", op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if reply == nil {
			return nil, nil
		}
		text, ok := reply.(string)
		if !ok {
			return nil, runner.Definite(fmt.Errorf("get %s: unexpected reply %v", op.K, reply))
		}
		return text, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func classify(err error) error {
	if err == nil {
		return nil
	}
	var dial *net.OpError
	if notApplied(err) || (errors.As(err, &dial) && dial.Op == "dial") {
		return runner.Definite(err)
	}
	return err
}

func (c *client) call(ctx context.Context, base string, args ...string) (any, error) {
	reply, err := request(ctx, base, args...)
	var redirect replyError
	if errors.As(err, &redirect) && strings.HasPrefix(string(redirect), "TRY ") {
		return request(ctx, strings.TrimPrefix(string(redirect), "TRY "), args...)
	}
	return reply, err
}

func request(ctx context.Context, address string, args ...string) (any, error) {
	dialer := net.Dialer{Timeout: time.Second}
	conn, err := dialer.DialContext(ctx, "tcp", address)
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	if deadline, ok := ctx.Deadline(); ok {
		_ = conn.SetDeadline(deadline)
	}
	var out strings.Builder
	fmt.Fprintf(&out, "*%d\r\n", len(args))
	for _, arg := range args {
		fmt.Fprintf(&out, "$%d\r\n%s\r\n", len(arg), arg)
	}
	if _, err := io.WriteString(conn, out.String()); err != nil {
		return nil, err
	}
	return readReply(bufio.NewReader(conn))
}

func readReply(r *bufio.Reader) (any, error) {
	line, err := r.ReadString('\n')
	if err != nil {
		return nil, err
	}
	line = strings.TrimSuffix(line, "\r\n")
	if line == "" {
		return nil, errors.New("empty reply")
	}
	body := line[1:]
	switch line[0] {
	case '+':
		return body, nil
	case '-':
		return nil, replyError(body)
	case ':':
		return strconv.ParseInt(body, 10, 64)
	case '$':
		size, err := strconv.Atoi(body)
		if err != nil {
			return nil, err
		}
		if size < 0 {
			return nil, nil
		}
		data := make([]byte, size+2)
		if _, err := io.ReadFull(r, data); err != nil {
			return nil, err
		}
		return string(data[:size]), nil
	case '*':
		count, err := strconv.Atoi(body)
		if err != nil {
			return nil, err
		}
		if count < 0 {
			return nil, nil
		}
		items := make([]any, count)
		for i := range items {
			if items[i], err = readReply(r); err != nil {
				return nil, err
			}
		}
		return items, nil
	}
	return nil, fmt.Errorf("unexpected reply %q", line)
}

// The cluster is ready once every server names the same leader and that
// leader applies a write.
func (a adapter) Ready(ctx context.Context) error {
	var leader string
	for _, base := range a.servers {
		reply, err := request(ctx, base, "RAFTLEADER")
		if err != nil {
			return fmt.Errorf("%s: %w", base, err)
		}
		name, _ := reply.(string)
		if name == "" || (leader != "" && name != leader) {
			return fmt.Errorf("%s: leader %q, expected %q", base, name, leader)
		}
		leader = name
	}
	_, err := request(ctx, leader, "SET", "ready", "1")
	return err
}

func waitLeader(address string) {
	for {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		reply, err := request(ctx, address, "RAFTLEADER")
		cancel()
		if name, _ := reply.(string); err == nil && name != "" {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func main() {
	if len(os.Args) == 3 && os.Args[1] == "wait-leader" {
		waitLeader(os.Args[2])
		return
	}
	servers := make([]string, 3)
	for i := range servers {
		servers[i] = fmt.Sprintf("10.77.0.%d:7481", i+1)
	}
	runner.Main(adapter{servers: servers})
}
