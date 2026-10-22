// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bufio"
	"bytes"
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

const (
	formedPath = "/tmp/redisraft/formed"
	casScript  = "if redis.call('GET', KEYS[1]) == ARGV[1] then redis.call('SET', KEYS[1], ARGV[2]) return 1 end return 0"
)

var nodes = []string{"10.77.0.1:6379", "10.77.0.2:6379", "10.77.0.3:6379"}

type adapter struct{}

func (adapter) Family() family.Family { return family.Register{} }

func (adapter) Connect(context.Context, int) (runner.Client, error) { return client{}, nil }

type client struct{}

func (client) Close() {}

type serverError string

func (e serverError) Error() string { return string(e) }

// Redis-Raft rejects a command with these errors before it appends the
// command to the Raft log.
var rejected = []string{"MOVED ", "NOLEADER", "NOCLUSTER", "LOADING", "ERR not leader", "CLUSTERDOWN ", "TRYAGAIN"}

func definite(err error) bool {
	var reply serverError
	if !errors.As(err, &reply) {
		return false
	}
	text := strings.TrimLeft(string(reply), "-")
	for _, prefix := range rejected {
		if strings.HasPrefix(text, prefix) {
			return true
		}
	}
	return false
}

// Each operation goes to one node drawn from the SDK stream. A follower
// answers with -MOVED and the leader's address; the client follows one
// redirect.
func (c client) Invoke(ctx context.Context, op family.Op) (any, error) {
	addr := nodes[sdk.Below(uint64(len(nodes)))]
	switch op.F {
	case family.RegisterWrite:
		_, err := call(ctx, addr, "SET", op.K, op.V.(string))
		if definite(err) {
			return nil, runner.Definite(err)
		}
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		r, err := call(ctx, addr, "EVAL", casScript, "1", op.K, pair[0], pair[1])
		if definite(err) {
			return nil, runner.Definite(err)
		}
		if err != nil {
			return nil, err
		}
		if r.num != 1 {
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return nil, nil
	case family.RegisterReadF:
		r, err := call(ctx, addr, "GET", op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if r.null {
			return nil, nil
		}
		return r.str, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

type reply struct {
	str   string
	num   int64
	null  bool
	elems []reply
}

// A failed dial is definite. Any later failure may follow the server's
// receipt of the command.
func call(ctx context.Context, addr string, args ...string) (reply, error) {
	r, err := once(ctx, addr, args)
	var moved serverError
	if errors.As(err, &moved) && strings.HasPrefix(string(moved), "MOVED ") {
		fields := strings.Fields(string(moved))
		leader := fields[len(fields)-1]
		return once(ctx, leader, args)
	}
	return r, err
}

func once(ctx context.Context, addr string, args []string) (reply, error) {
	conn, err := (&net.Dialer{Timeout: time.Second}).DialContext(ctx, "tcp", addr)
	if err != nil {
		return reply{}, runner.Definite(err)
	}
	defer conn.Close()
	if deadline, ok := ctx.Deadline(); ok {
		if err := conn.SetDeadline(deadline); err != nil {
			return reply{}, err
		}
	}
	var b bytes.Buffer
	fmt.Fprintf(&b, "*%d\r\n", len(args))
	for _, arg := range args {
		fmt.Fprintf(&b, "$%d\r\n%s\r\n", len(arg), arg)
	}
	if _, err := conn.Write(b.Bytes()); err != nil {
		return reply{}, fmt.Errorf("%s: %w", addr, err)
	}
	r, err := parse(bufio.NewReader(conn))
	if err != nil {
		return reply{}, fmt.Errorf("%s: %w", addr, err)
	}
	return r, nil
}

func parse(in *bufio.Reader) (reply, error) {
	line, err := in.ReadString('\n')
	if err != nil {
		return reply{}, err
	}
	line = strings.TrimSuffix(line, "\r\n")
	if line == "" {
		return reply{}, errors.New("empty reply line")
	}
	body := line[1:]
	switch line[0] {
	case '+':
		return reply{str: body}, nil
	case '-':
		return reply{}, serverError(body)
	case ':':
		n, err := strconv.ParseInt(body, 10, 64)
		return reply{num: n}, err
	case '$':
		n, err := strconv.Atoi(body)
		if err != nil {
			return reply{}, err
		}
		if n < 0 {
			return reply{null: true}, nil
		}
		data := make([]byte, n+2)
		if _, err := io.ReadFull(in, data); err != nil {
			return reply{}, err
		}
		return reply{str: string(data[:n])}, nil
	case '*':
		n, err := strconv.Atoi(body)
		if err != nil {
			return reply{}, err
		}
		if n < 0 {
			return reply{null: true}, nil
		}
		r := reply{elems: make([]reply, n)}
		for i := range r.elems {
			if r.elems[i], err = parse(in); err != nil {
				return reply{}, err
			}
		}
		return r, nil
	}
	return reply{}, fmt.Errorf("unknown reply %q", line)
}

// Redis-Raft before the Redis 7 module API answers RAFT.INFO; later versions
// report the same fields, prefixed with raft_, in INFO raft.
func info(ctx context.Context, addr string) (map[string]string, error) {
	r, err := once(ctx, addr, []string{"RAFT.INFO"})
	var unknown serverError
	if errors.As(err, &unknown) {
		r, err = once(ctx, addr, []string{"INFO", "raft"})
	}
	if err != nil {
		return nil, err
	}
	fields := map[string]string{}
	for _, line := range strings.Split(r.str, "\r\n") {
		if name, value, ok := strings.Cut(line, ":"); ok {
			fields[strings.TrimPrefix(name, "raft_")] = value
		}
	}
	return fields, nil
}

// Ready forms the cluster once per execution: n1 initializes it and the
// other nodes join through n1. The joins finish in the background, and a
// joined node votes once it has caught up. The cluster is ready when n1 sees
// three voting nodes and a write succeeds. Later probes only write.
func (adapter) Ready(ctx context.Context) error {
	if _, err := os.Stat(formedPath); err != nil {
		if err := form(ctx); err != nil {
			return err
		}
		if err := os.WriteFile(formedPath, nil, 0o644); err != nil {
			return err
		}
	}
	var errs []error
	for _, addr := range nodes {
		if _, err := call(ctx, addr, "SET", "ready", "1"); err != nil {
			errs = append(errs, err)
			continue
		}
		return nil
	}
	return errors.Join(errs...)
}

func member(err error) bool {
	var reply serverError
	return err == nil || errors.As(err, &reply) && strings.Contains(string(reply), "Already cluster member")
}

func form(ctx context.Context) error {
	if _, err := once(ctx, nodes[0], []string{"RAFT.CLUSTER", "INIT"}); !member(err) {
		return fmt.Errorf("init: %w", err)
	}
	for _, addr := range nodes[1:] {
		if _, err := once(ctx, addr, []string{"RAFT.CLUSTER", "JOIN", nodes[0]}); !member(err) {
			return fmt.Errorf("join %s: %w", addr, err)
		}
	}
	for {
		fields, err := info(ctx, nodes[0])
		if err == nil && fields["num_voting_nodes"] == strconv.Itoa(len(nodes)) {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("cluster formation: %v: %w", fields, ctx.Err())
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func main() { runner.Main(adapter{}) }
