// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const tokenFile = "/tmp/bao/token"

var errNotFound = errors.New("not found")

type adapter struct {
	nodes []string
	http  *http.Client
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	token, err := readToken()
	if err != nil {
		return nil, err
	}
	return &client{adapter: a, token: token, version: map[string]int64{}}, nil
}

func readToken() (string, error) {
	data, err := os.ReadFile(tokenFile)
	if err != nil {
		return "", err
	}
	token := strings.TrimSpace(string(data))
	if token == "" {
		return "", errors.New("empty token file")
	}
	return token, nil
}

type client struct {
	adapter
	token   string
	mu      sync.Mutex
	version map[string]int64
}

func (c *client) Close() {}

// Values are unique per operation, so the KV version that stored a value
// names the one write that stored it.
func (c *client) remember(key, value string, version int64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.version[key+"\x00"+value] = version
}

func (c *client) known(key, value string) (int64, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	version, ok := c.version[key+"\x00"+value]
	return version, ok
}

// Every node accepts every request: a standby forwards it to the active node,
// so each operation goes to one node drawn from the SDK stream.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	node := c.nodes[sdk.Below(uint64(len(c.nodes)))]
	switch op.F {
	case family.RegisterWrite:
		value := op.V.(string)
		version, err := c.put(ctx, node, op.K, value, nil)
		if err != nil {
			return nil, err
		}
		c.remember(op.K, value, version)
		return nil, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		expected, ok := c.known(op.K, pair[0])
		if !ok {
			current, version, err := c.get(ctx, node, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if current == nil || *current != pair[0] {
				return nil, runner.Definite(errors.New("cas: current value differs"))
			}
			expected = version
		}
		version, err := c.put(ctx, node, op.K, pair[1], &expected)
		if err != nil {
			return nil, err
		}
		c.remember(op.K, pair[1], version)
		return nil, nil
	case family.RegisterReadF:
		current, _, err := c.get(ctx, node, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if current == nil {
			return nil, nil
		}
		return *current, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

type response struct {
	Errors []string `json:"errors"`
	Data   struct {
		Version  int64             `json:"version"`
		Data     map[string]string `json:"data"`
		Metadata struct {
			Version int64 `json:"version"`
		} `json:"metadata"`
	} `json:"data"`
}

// A request that fails before it is sent cannot have written, and a
// check-and-set conflict is an answer from the active node. Every other write
// failure may follow a committed write.
func (c *client) put(ctx context.Context, node, key, value string, cas *int64) (int64, error) {
	body := map[string]any{"data": map[string]string{"v": value}}
	if cas != nil {
		body["options"] = map[string]int64{"cas": *cas}
	}
	encoded, err := json.Marshal(body)
	if err != nil {
		return 0, runner.Definite(err)
	}
	status, decoded, err := c.do(ctx, http.MethodPost, node, key, encoded)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return 0, runner.Definite(err)
		}
		return 0, err
	}
	if status == http.StatusOK {
		return decoded.Data.Version, nil
	}
	err = fmt.Errorf("put %s: status %d: %s", key, status, strings.Join(decoded.Errors, "; "))
	if status == http.StatusBadRequest && strings.Contains(err.Error(), "check-and-set parameter did not match") {
		return 0, runner.Definite(err)
	}
	return 0, err
}

func (c *client) get(ctx context.Context, node, key string) (*string, int64, error) {
	status, decoded, err := c.do(ctx, http.MethodGet, node, key, nil)
	if err != nil {
		return nil, 0, err
	}
	if status == http.StatusNotFound && len(decoded.Errors) == 0 {
		return nil, 0, nil
	}
	if status != http.StatusOK {
		return nil, 0, fmt.Errorf("get %s: status %d: %s", key, status, strings.Join(decoded.Errors, "; "))
	}
	value, ok := decoded.Data.Data["v"]
	if !ok {
		return nil, 0, fmt.Errorf("get %s: %w", key, errNotFound)
	}
	c.remember(key, value, decoded.Data.Metadata.Version)
	return &value, decoded.Data.Metadata.Version, nil
}

func (c *client) do(ctx context.Context, method, node, key string, body []byte) (int, response, error) {
	var decoded response
	request, err := http.NewRequestWithContext(ctx, method, "http://"+node+"/v1/secret/data/"+key, bytes.NewReader(body))
	if err != nil {
		return 0, decoded, err
	}
	request.Header.Set("X-Vault-Token", c.token)
	request.Header.Set("Content-Type", "application/json")
	reply, err := c.http.Do(request)
	if err != nil {
		return 0, decoded, err
	}
	defer reply.Body.Close()
	data, err := io.ReadAll(reply.Body)
	if err != nil {
		return 0, decoded, err
	}
	if len(data) > 0 {
		if err := json.Unmarshal(data, &decoded); err != nil {
			return 0, decoded, fmt.Errorf("decode %d response: %w", reply.StatusCode, err)
		}
	}
	return reply.StatusCode, decoded, nil
}

func (a adapter) Ready(ctx context.Context) error {
	token, err := readToken()
	if err != nil {
		return err
	}
	c := &client{adapter: a, token: token, version: map[string]int64{}}
	var errs []error
	for _, node := range a.nodes {
		_, err := c.put(ctx, node, "ready", "1", nil)
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s: %w", node, err))
	}
	return errors.Join(errs...)
}

func main() {
	nodes := make([]string, 3)
	for i := range nodes {
		nodes[i] = fmt.Sprintf("10.77.0.%d:8200", i+1)
	}
	transport := &http.Transport{
		Proxy:             nil,
		DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives: true,
	}
	// A standby whose forwarded request fails answers 307 toward the active
	// node, and the forwarded request may already have committed. Following
	// the redirect would resend it and report the second hop's dial error.
	noRedirect := func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	runner.Main(adapter{nodes: nodes, http: &http.Client{Transport: transport, CheckRedirect: noRedirect}})
}
