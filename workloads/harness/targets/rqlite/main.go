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
	"strconv"
	"strings"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	upsertSQL = "INSERT INTO kv(k, v) VALUES(?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v"
	casSQL    = "UPDATE kv SET v = ? WHERE k = ? AND v = ?"
	readSQL   = "SELECT v FROM kv WHERE k = ?"
	appendSQL = "INSERT INTO log(v) VALUES(?)"
	scanSQL   = "SELECT v FROM log ORDER BY id"
	readLevel = "linearizable"
	modeEnv   = "HARNESS_RQLITE_MODE"
)

var nodes = []string{
	"http://127.0.0.1:4001",
	"http://127.0.0.1:4003",
	"http://127.0.0.1:4005",
}

var readySQL = [][]any{
	{"CREATE TABLE IF NOT EXISTS kv (k INTEGER PRIMARY KEY, v TEXT)"},
	{"CREATE TABLE IF NOT EXISTS log (id INTEGER PRIMARY KEY, v TEXT)"},
	{"CREATE TABLE IF NOT EXISTS ready (k INTEGER PRIMARY KEY, n INTEGER)"},
	{"INSERT INTO ready(k, n) VALUES(1, 1) ON CONFLICT(k) DO UPDATE SET n = n + 1"},
}

type adapter struct {
	forward bool
}

func (a adapter) Family() family.Family {
	if a.forward {
		return family.Log{}
	}
	return family.Register{}
}

type client struct {
	base    string
	http    *http.Client
	forward bool
}

type result struct {
	Values       [][]any `json:"values"`
	RowsAffected int64   `json:"rows_affected"`
	LastInsertID int64   `json:"last_insert_id"`
	Error        string  `json:"error"`
}

type response struct {
	Results []result `json:"results"`
	Error   string   `json:"error"`
	text    string
}

func newClient(base string) *client {
	return &client{
		base: base,
		http: &http.Client{
			Transport: &http.Transport{
				Proxy:             nil,
				DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
				DisableKeepAlives: true,
			},
			CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
		},
	}
}

func (a adapter) Connect(_ context.Context, _ int) (runner.Client, error) {
	if a.forward {
		return forwardClient{}, nil
	}
	return newClient(nodes[sdk.Below(uint64(len(nodes)))]), nil
}

type forwardClient struct{}

func (forwardClient) Close() {}

func (forwardClient) Invoke(ctx context.Context, op family.Op) (any, error) {
	c := newClient(nodes[sdk.Below(uint64(len(nodes)))])
	c.forward = true
	defer c.Close()
	switch op.F {
	case family.LogAppend:
		return c.appendValue(ctx, op.V.(string))
	case family.LogRead:
		values, err := c.scan(ctx)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return values, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func (c *client) appendValue(ctx context.Context, value string) (any, error) {
	resp, status, err := c.post(ctx, "/db/execute", [][]any{{appendSQL, value}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		err := fmt.Errorf("append: status %d: %s", status, resp.text)
		if refused(resp.text) {
			return nil, runner.Definite(err)
		}
		return nil, err
	}
	if resp.Error != "" {
		err := fmt.Errorf("append: %s", resp.Error)
		if refused(resp.Error) {
			return nil, runner.Definite(err)
		}
		return nil, err
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("append: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("append: %s", res.Error)
	}
	if res.LastInsertID <= 0 {
		return nil, fmt.Errorf("append: last_insert_id %d", res.LastInsertID)
	}
	return uint64(res.LastInsertID), nil
}

// These are the errors a client would read as "the write was not applied":
// no leader, or the node could not reach the leader to forward it. The log
// checker reports any such append that a later read contains.
func refused(text string) bool {
	return text == "leader not found" || text == "not leader" || strings.Contains(text, "dial tcp")
}

func (c *client) scan(ctx context.Context) ([]string, error) {
	resp, status, err := c.post(ctx, "/db/query?level="+readLevel, [][]any{{scanSQL}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("scan: status %d: %s", status, resp.text)
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("scan: %s", resp.Error)
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("scan: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("scan: %s", res.Error)
	}
	values := make([]string, 0, len(res.Values))
	for _, row := range res.Values {
		if len(row) != 1 {
			return nil, fmt.Errorf("scan: row has %d columns", len(row))
		}
		value, ok := row[0].(string)
		if !ok {
			return nil, fmt.Errorf("scan: value %v has type %T", row[0], row[0])
		}
		values = append(values, value)
	}
	return values, nil
}

func (c *client) Close() { c.http.CloseIdleConnections() }

func key(k string) (int64, error) {
	return strconv.ParseInt(strings.TrimPrefix(k, "k"), 10, 64)
}

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	k, err := key(op.K)
	if err != nil {
		return nil, runner.Definite(err)
	}
	switch op.F {
	case family.RegisterWrite:
		_, err := c.execute(ctx, [][]any{{upsertSQL, k, op.V.(string)}})
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		res, err := c.execute(ctx, [][]any{{casSQL, pair[1], k, pair[0]}})
		if err != nil {
			return nil, err
		}
		switch res.RowsAffected {
		case 0:
			return nil, runner.Definite(errors.New("cas: current value differs"))
		case 1:
			return nil, nil
		}
		return nil, fmt.Errorf("cas: rows_affected %d", res.RowsAffected)
	case family.RegisterReadF:
		value, err := c.read(ctx, k)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return value, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func (c *client) read(ctx context.Context, k int64) (any, error) {
	resp, status, err := c.post(ctx, "/db/query?level="+readLevel, [][]any{{readSQL, k}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("query: status %d: %s", status, resp.text)
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("query: %s", resp.Error)
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("query: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("query: %s", res.Error)
	}
	switch len(res.Values) {
	case 0:
		return nil, nil
	case 1:
		if len(res.Values[0]) != 1 {
			return nil, fmt.Errorf("query: row has %d columns", len(res.Values[0]))
		}
		switch value := res.Values[0][0].(type) {
		case nil:
			return nil, nil
		case string:
			return value, nil
		default:
			return nil, fmt.Errorf("query: value %v has type %T", value, value)
		}
	}
	return nil, fmt.Errorf("query: %d rows for one key", len(res.Values))
}

func (c *client) execute(ctx context.Context, statements [][]any) (result, error) {
	results, err := c.executeAll(ctx, statements)
	if err != nil {
		return result{}, err
	}
	return results[0], nil
}

func (c *client) executeAll(ctx context.Context, statements [][]any) ([]result, error) {
	resp, status, err := c.post(ctx, "/db/execute", statements)
	if err != nil {
		return nil, err
	}
	switch {
	case status == http.StatusOK:
	case status == http.StatusInternalServerError && resp.text != "leader not found":
		return nil, fmt.Errorf("execute: status %d: %s", status, resp.text)
	default:
		return nil, runner.Definite(fmt.Errorf("execute: status %d: %s", status, resp.text))
	}
	if resp.Error == "not leader" || resp.Error == "store not open" {
		return nil, runner.Definite(fmt.Errorf("execute: %s", resp.Error))
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("execute: %s", resp.Error)
	}
	if len(resp.Results) != len(statements) {
		return nil, fmt.Errorf("execute: %d results for %d statements", len(resp.Results), len(statements))
	}
	for _, res := range resp.Results {
		if res.Error != "" {
			return nil, runner.Definite(fmt.Errorf("execute: %s", res.Error))
		}
	}
	return resp.Results, nil
}

func (c *client) post(ctx context.Context, path string, statements [][]any) (response, int, error) {
	body, err := json.Marshal(statements)
	if err != nil {
		return response{}, 0, runner.Definite(err)
	}
	if c.forward {
		resp, status, _, err := c.send(ctx, c.base+path, body)
		return resp, status, err
	}
	separator := "?"
	if strings.Contains(path, "?") {
		separator = "&"
	}
	target := c.base + path + separator + "redirect"
	for hop := 0; ; hop++ {
		resp, status, location, err := c.send(ctx, target, body)
		if err != nil || status != http.StatusMovedPermanently || hop > 0 || location == "" {
			return resp, status, err
		}
		target = location
	}
}

func (c *client) send(ctx context.Context, target string, body []byte) (response, int, string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, bytes.NewReader(body))
	if err != nil {
		return response{}, 0, "", runner.Definite(err)
	}
	req.Header.Set("Content-Type", "application/json")
	httpResp, err := c.http.Do(req)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return response{}, 0, "", runner.Definite(err)
		}
		return response{}, 0, "", err
	}
	defer httpResp.Body.Close()
	text, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return response{}, 0, "", err
	}
	resp := response{text: strings.TrimSpace(string(text))}
	if httpResp.StatusCode == http.StatusOK {
		if err := json.Unmarshal(text, &resp); err != nil {
			return response{}, 0, "", fmt.Errorf("decode %q: %w", text, err)
		}
	}
	return resp, httpResp.StatusCode, httpResp.Header.Get("Location"), nil
}

func (adapter) Ready(ctx context.Context) error {
	var errs []error
	for _, base := range nodes {
		c := newClient(base)
		_, err := c.executeAll(ctx, readySQL)
		c.Close()
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s: %w", base, err))
	}
	return errors.Join(errs...)
}

func main() {
	switch mode := os.Getenv(modeEnv); mode {
	case "":
		runner.Main(adapter{})
	case "forward":
		runner.Main(adapter{forward: true})
	default:
		fmt.Fprintf(os.Stderr, "unknown %s %q\n", modeEnv, mode)
		os.Exit(2)
	}
}
