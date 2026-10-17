package main

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	subakofn "github.com/subako-ai/subako-function-sdk-go"
)

// volumeTransport sends git's HTTP requests through the worker. Pack data is
// binary, which the worker carries only as volume files, so each request body
// is spooled to a file under dir and each response body is saved to one.
//
// The worker is told not to follow redirects, so net/http follows them and
// go-git's redirect policy, which strips credentials across origins, applies.
type volumeTransport struct {
	dir  string
	send func(subakofn.Request) (*subakofn.Response, error)
}

func (t volumeTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Body != nil {
		defer req.Body.Close()
	}
	if err := os.MkdirAll(t.dir, 0o755); err != nil {
		return nil, err
	}
	name, err := spoolName()
	if err != nil {
		return nil, err
	}
	follow := false
	sent := subakofn.Request{
		Method:          req.Method,
		URL:             req.URL.String(),
		Headers:         headers(req.Header),
		Output:          subakofn.ToFile(filepath.Join(t.dir, name+".response")),
		FollowRedirects: &follow,
	}
	if sent.Method == "" {
		sent.Method = http.MethodGet
	}
	if req.Body != nil && req.Body != http.NoBody {
		path := filepath.Join(t.dir, name+".request")
		if err := spool(path, req.Body); err != nil {
			return nil, err
		}
		defer os.Remove(path)
		sent.Body = subakofn.FileBody(path)
	}
	if deadline, ok := req.Context().Deadline(); ok {
		left := time.Until(deadline).Milliseconds()
		if left < 1 {
			return nil, req.Context().Err()
		}
		sent.TimeoutMS = uint64(left)
	}

	res, err := t.send(sent)
	if err != nil {
		return nil, err
	}
	body, length, err := responseBody(res.Body)
	if err != nil {
		return nil, err
	}
	header := make(http.Header, len(res.Headers))
	for key, value := range res.Headers {
		header.Set(key, value)
	}
	return &http.Response{
		Status:        fmt.Sprintf("%d %s", res.Status, http.StatusText(res.Status)),
		StatusCode:    res.Status,
		Proto:         "HTTP/1.1",
		ProtoMajor:    1,
		ProtoMinor:    1,
		Header:        header,
		Body:          body,
		ContentLength: length,
		Request:       req,
	}, nil
}

func spoolName() (string, error) {
	var id [12]byte
	if _, err := rand.Read(id[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(id[:]), nil
}

func spool(path string, body io.Reader) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	_, err = io.Copy(f, body)
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	return err
}

// headers flattens h the way the worker takes it: lowercase names, repeated
// values joined with ", ".
func headers(h http.Header) map[string]string {
	if len(h) == 0 {
		return nil
	}
	flat := make(map[string]string, len(h))
	for key, values := range h {
		flat[strings.ToLower(key)] = strings.Join(values, ", ")
	}
	return flat
}

func responseBody(body subakofn.ResponseBody) (io.ReadCloser, int64, error) {
	switch body.Type {
	case "file":
		f, err := os.Open(body.Path)
		if err != nil {
			return nil, 0, err
		}
		return removeOnClose{f}, int64(body.Bytes), nil
	case "text":
		return io.NopCloser(strings.NewReader(body.Text)), int64(len(body.Text)), nil
	default:
		return nil, 0, fmt.Errorf("the worker answered with a %q body", body.Type)
	}
}

// removeOnClose deletes the spooled response once git has read it.
type removeOnClose struct{ *os.File }

func (f removeOnClose) Close() error {
	err := f.File.Close()
	if removeErr := os.Remove(f.Name()); err == nil {
		err = removeErr
	}
	return err
}
