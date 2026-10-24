// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const membersEnv = "HARNESS_ETCD_MEMBERS"

type adapter struct {
	members []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return client{members: a.members, http: newHTTP()}, nil
}

type client struct {
	members []string
	http    *http.Client
}

func newHTTP() *http.Client {
	return &http.Client{Transport: &http.Transport{
		Proxy:             nil,
		DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives: true,
	}}
}

func (c client) Close() {}

type kv struct {
	Value string `json:"value"`
}

// The gateway omits zero fields: a range with no match has no "kvs" and a
// transaction whose compare failed has no "succeeded".
type reply struct {
	Kvs       []kv   `json:"kvs"`
	Succeeded bool   `json:"succeeded"`
	Error     string `json:"error"`
	Message   string `json:"message"`
}

func enc(s string) string { return base64.StdEncoding.EncodeToString([]byte(s)) }

// Each operation goes to one member drawn from the SDK stream, so reads and
// writes reach followers as well as the leader.
func (c client) Invoke(ctx context.Context, op family.Op) (any, error) {
	base := c.members[sdk.Below(uint64(len(c.members)))]
	key := enc(op.K)
	switch op.F {
	case family.RegisterWrite:
		_, err := c.post(ctx, base, "/v3/kv/put", map[string]any{"key": key, "value": enc(op.V.(string))})
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		r, err := c.post(ctx, base, "/v3/kv/txn", map[string]any{
			"compare": []any{map[string]any{"key": key, "result": "EQUAL", "target": "VALUE", "value": enc(pair[0])}},
			"success": []any{map[string]any{"request_put": map[string]any{"key": key, "value": enc(pair[1])}}},
		})
		if err != nil {
			return nil, err
		}
		if !r.Succeeded {
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return nil, nil
	case family.RegisterReadF:
		r, err := c.post(ctx, base, "/v3/kv/range", map[string]any{"key": key})
		if err != nil {
			return nil, runner.Definite(err)
		}
		switch len(r.Kvs) {
		case 0:
			return nil, nil
		case 1:
			value, err := base64.StdEncoding.DecodeString(r.Kvs[0].Value)
			if err != nil {
				return nil, runner.Definite(err)
			}
			return string(value), nil
		}
		return nil, runner.Definite(fmt.Errorf("range: %d values for one key", len(r.Kvs)))
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// Only a refused or failed dial is definite. The gateway answers every other
// error after the request may have reached the raft log.
func (c client) post(ctx context.Context, base, path string, body any) (reply, error) {
	data, err := json.Marshal(body)
	if err != nil {
		return reply{}, runner.Definite(err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, base+path, bytes.NewReader(data))
	if err != nil {
		return reply{}, runner.Definite(err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return reply{}, runner.Definite(err)
		}
		return reply{}, err
	}
	defer resp.Body.Close()
	text, err := io.ReadAll(resp.Body)
	if err != nil {
		return reply{}, err
	}
	var r reply
	if err := json.Unmarshal(text, &r); err != nil {
		return reply{}, fmt.Errorf("%s: status %d: %q", path, resp.StatusCode, text)
	}
	if resp.StatusCode != http.StatusOK || r.Error != "" || r.Message != "" {
		return reply{}, fmt.Errorf("%s: status %d: %s", path, resp.StatusCode, strings.TrimSpace(string(text)))
	}
	return r, nil
}

func (a adapter) Ready(ctx context.Context) error {
	c := client{http: newHTTP()}
	var errs []error
	for _, base := range a.members {
		_, err := c.post(ctx, base, "/v3/kv/put", map[string]any{"key": enc("ready"), "value": enc("1")})
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s: %w", base, err))
	}
	return errors.Join(errs...)
}

func main() {
	count := 3
	if text := os.Getenv(membersEnv); text != "" {
		n, err := strconv.Atoi(text)
		if err != nil || n < 1 {
			fmt.Fprintf(os.Stderr, "bad %s %q\n", membersEnv, text)
			os.Exit(2)
		}
		count = n
	}
	members := []string{"http://127.0.0.1:2379"}
	if count > 1 {
		members = make([]string, count)
		for i := range members {
			members[i] = fmt.Sprintf("http://10.77.0.%d:2379", i+1)
		}
	}
	runner.Main(adapter{members: members})
}
