package main

import (
	"io"
	"net/http"
	"net/http/cgi"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	subakofn "github.com/subako-ai/subako-function-sdk-go"
)

// run runs the git CLI in dir, which the tests use to stand up and inspect the
// remote side.
func run(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=Upstream", "GIT_AUTHOR_EMAIL=upstream@example.com",
		"GIT_COMMITTER_NAME=Upstream", "GIT_COMMITTER_EMAIL=upstream@example.com",
		"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func write(t *testing.T, path, text string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
		t.Fatal(err)
	}
}

// remote serves a bare repository, site.git, with a two-file site on main and
// a dev branch, through git http-backend. It returns the repository's URL and
// its directory.
func remote(t *testing.T) (string, string) {
	t.Helper()
	root := t.TempDir()
	seed := t.TempDir()
	run(t, seed, "init", "-q", "-b", "main")
	write(t, filepath.Join(seed, "index.html"), "<h1>Hello</h1>\n<p>Welcome.</p>\n")
	write(t, filepath.Join(seed, "style.css"), "h1 { color: navy; }\n")
	write(t, filepath.Join(seed, "deploy.sh"), "#!/bin/sh\necho deploy\n")
	if err := os.Chmod(filepath.Join(seed, "deploy.sh"), 0o755); err != nil {
		t.Fatal(err)
	}
	run(t, seed, "add", ".")
	run(t, seed, "commit", "-q", "-m", "First page")
	run(t, seed, "branch", "dev")
	bare := filepath.Join(root, "site.git")
	run(t, root, "clone", "-q", "--bare", seed, bare)
	run(t, bare, "config", "http.receivepack", "true")

	server := httptest.NewServer(&cgi.Handler{
		Path: gitPath(t),
		Args: []string{"http-backend"},
		Env:  []string{"GIT_PROJECT_ROOT=" + root, "GIT_HTTP_EXPORT_ALL=1", "GIT_CONFIG_NOSYSTEM=1"},
	})
	t.Cleanup(server.Close)
	return server.URL + "/site.git", bare
}

func gitPath(t *testing.T) string {
	t.Helper()
	path, err := exec.LookPath("git")
	if err != nil {
		t.Skip("the tests need the git CLI for the remote side")
	}
	return path
}

// worker stands in for the Subako worker: it sends each request with net/http,
// reading a file body from the volume and saving the response to the volume,
// as the worker does.
func worker(t *testing.T) func(subakofn.Request) (*subakofn.Response, error) {
	t.Helper()
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return func(req subakofn.Request) (*subakofn.Response, error) {
		if req.FollowRedirects == nil || *req.FollowRedirects {
			t.Errorf("%s %s asks the worker to follow redirects", req.Method, req.URL)
		}
		if req.Output == nil || req.Output.Type != "file" {
			t.Fatalf("%s %s does not save its response to a file", req.Method, req.URL)
		}
		var body io.Reader
		var length int64
		if req.Body != nil {
			if req.Body.Type != "file" {
				t.Fatalf("%s %s sends a %s body", req.Method, req.URL, req.Body.Type)
			}
			f, err := os.Open(req.Body.Path)
			if err != nil {
				return nil, err
			}
			defer f.Close()
			info, err := f.Stat()
			if err != nil {
				return nil, err
			}
			body, length = f, info.Size()
		}
		sent, err := http.NewRequest(req.Method, req.URL, body)
		if err != nil {
			return nil, err
		}
		sent.ContentLength = length
		for key, value := range req.Headers {
			sent.Header.Set(key, value)
		}
		res, err := client.Do(sent)
		if err != nil {
			return nil, &subakofn.HTTPError{Kind: subakofn.Connection, Message: err.Error()}
		}
		defer res.Body.Close()
		out, err := os.Create(req.Output.Path)
		if err != nil {
			return nil, err
		}
		n, err := io.Copy(out, res.Body)
		if closeErr := out.Close(); err == nil {
			err = closeErr
		}
		if err != nil {
			return nil, err
		}
		headers := map[string]string{}
		for key, values := range res.Header {
			headers[strings.ToLower(key)] = strings.Join(values, ", ")
		}
		return &subakofn.Response{
			Status:  res.StatusCode,
			Headers: headers,
			Body:    subakofn.ResponseBody{Type: "file", Path: req.Output.Path, Bytes: uint64(n)},
		}, nil
	}
}

// newTools builds the tools on a fake worker, with a spool directory the test
// checks is left empty.
func newTools(t *testing.T) tools {
	t.Helper()
	spool := filepath.Join(t.TempDir(), "spool")
	t.Cleanup(func() {
		entries, _ := os.ReadDir(spool)
		if len(entries) > 0 {
			t.Errorf("the spool directory still holds %d files", len(entries))
		}
	})
	return tools{http: &http.Client{Transport: volumeTransport{dir: spool, send: worker(t)}}}
}

func cloneSite(t *testing.T, tl tools, url string) string {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "site")
	_, cloned, err := tl.clone(t.Context(), nil, CloneInput{URL: url, Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if cloned.Head.Branch != "main" || cloned.Head.Subject != "First page" || cloned.Files != 3 {
		t.Fatalf("cloned %+v", cloned)
	}
	return dir
}

func TestCloneEditCommitPush(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)

	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n<p>Welcome.</p>\n")
	write(t, filepath.Join(dir, "about.html"), "<h1>About</h1>\n")
	if err := os.Remove(filepath.Join(dir, "style.css")); err != nil {
		t.Fatal(err)
	}
	wantChanges := []Change{{"about.html", "added"}, {"index.html", "modified"}, {"style.css", "deleted"}}

	_, status, err := tl.status(t.Context(), nil, DirInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(status.Changes, wantChanges) || !reflect.DeepEqual(status.Branches, []string{"main"}) {
		t.Errorf("status %+v", status)
	}

	_, diff, err := tl.diff(t.Context(), nil, DiffInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range []string{"-<h1>Hello</h1>", "+<h1>Hi</h1>", " <p>Welcome.</p>", "+++ b/about.html", "-h1 { color: navy; }", "+++ /dev/null"} {
		if !strings.Contains(diff.Diff, line+"\n") {
			t.Errorf("the diff lacks %q:\n%s", line, diff.Diff)
		}
	}

	_, committed, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Greet briefly", AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(committed.Changes, wantChanges) || committed.Head.Subject != "Greet briefly" {
		t.Errorf("committed %+v", committed)
	}

	_, pushed, err := tl.push(t.Context(), nil, PushInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if pushed.UpToDate || pushed.Branch != "main" || pushed.Commit != committed.Head.Commit {
		t.Errorf("pushed %+v", pushed)
	}
	if got := run(t, bare, "log", "-1", "--format=%H %an %s", "main"); got != committed.Head.Commit+" Agent Greet briefly" {
		t.Errorf("the remote's main is at %q", got)
	}
	if got := run(t, bare, "ls-tree", "--name-only", "main"); got != "about.html\ndeploy.sh\nindex.html" {
		t.Errorf("the remote's main holds %q", got)
	}

	_, again, err := tl.push(t.Context(), nil, PushInput{Dir: dir})
	if err != nil || !again.UpToDate {
		t.Errorf("a second push answered %+v, %v", again, err)
	}
}

func TestAShallowCloneCommitsAndPushes(t *testing.T) {
	url, bare := remote(t)
	other := t.TempDir()
	run(t, other, "clone", "-q", bare, ".")
	write(t, filepath.Join(other, "index.html"), "<h1>Hello there</h1>\n")
	run(t, other, "commit", "-q", "-am", "Greet warmly")
	run(t, other, "push", "-q", "origin", "main")

	tl := newTools(t)
	dir := filepath.Join(t.TempDir(), "site")
	if _, _, err := tl.clone(t.Context(), nil, CloneInput{URL: url, Dir: dir, Depth: 1}); err != nil {
		t.Fatal(err)
	}
	_, log, err := tl.log(t.Context(), nil, LogInput{Dir: dir})
	if err != nil || len(log.Commits) != 1 || log.Commits[0].Message != "Greet warmly" {
		t.Errorf("the shallow clone's log is %+v, %v", log, err)
	}

	write(t, filepath.Join(dir, "about.html"), "<h1>About</h1>\n")
	_, committed, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Add about", AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: dir}); err != nil {
		t.Fatal(err)
	}
	if got := run(t, bare, "rev-parse", "main"); got != committed.Head.Commit {
		t.Errorf("the remote's main is at %s", got)
	}
	run(t, bare, "fsck", "--no-progress")
}

func TestCommitTakesOnlyTheNamedPaths(t *testing.T) {
	url, _ := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n")
	write(t, filepath.Join(dir, "style.css"), "h1 { color: teal; }\n")

	_, committed, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Recolor", Paths: []string{"style.css"}, AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(committed.Changes, []Change{{"style.css", "modified"}}) {
		t.Errorf("committed %+v", committed.Changes)
	}
	_, status, err := tl.status(t.Context(), nil, DirInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(status.Changes, []Change{{"index.html", "modified"}}) {
		t.Errorf("left uncommitted %+v", status.Changes)
	}
}

// On WASI a clone's scripts read back without their executable bit, and the
// clone has core.fileMode off.
func TestAMissingExecutableBitIsNoChange(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	run(t, dir, "config", "core.fileMode", "false")
	script := filepath.Join(dir, "deploy.sh")
	if err := os.Chmod(script, 0o644); err != nil {
		t.Fatal(err)
	}

	_, status, err := tl.status(t.Context(), nil, DirInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if len(status.Changes) != 0 {
		t.Errorf("a script without its executable bit shows as %+v", status.Changes)
	}

	write(t, script, "#!/bin/sh\necho deploy everything\n")
	if _, _, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Deploy everything", AuthorName: "Agent", AuthorEmail: "agent@example.com"}); err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: dir}); err != nil {
		t.Fatal(err)
	}
	if got := run(t, bare, "ls-tree", "main", "deploy.sh"); !strings.HasPrefix(got, "100755 ") {
		t.Errorf("the pushed script is %q", got)
	}
}

func TestCommitRefusesWhatItCannotRecord(t *testing.T) {
	url, _ := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)

	_, _, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Nothing", AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err == nil || !strings.Contains(err.Error(), "nothing to commit") {
		t.Errorf("an empty commit answered %v", err)
	}

	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n")
	_, _, err = tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Anonymous"})
	if err == nil || !strings.Contains(err.Error(), "author_name") {
		t.Errorf("a commit without an author answered %v", err)
	}
}

func TestANewBranchTakesTheChangesAndPushes(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n")

	_, out, err := tl.checkout(t.Context(), nil, CheckoutInput{Dir: dir, Branch: "greeting", Create: true})
	if err != nil {
		t.Fatal(err)
	}
	if out.Head.Branch != "greeting" {
		t.Errorf("checked out %+v", out)
	}
	_, committed, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Greet briefly", AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: dir}); err != nil {
		t.Fatal(err)
	}
	if got := run(t, bare, "rev-parse", "greeting"); got != committed.Head.Commit {
		t.Errorf("the remote's greeting is at %s", got)
	}
	if got := run(t, bare, "log", "-1", "--format=%s", "main"); got != "First page" {
		t.Errorf("the remote's main moved to %q", got)
	}
}

func TestCheckoutFindsARemoteBranchAndKeepsChangesSafe(t *testing.T) {
	url, _ := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)

	_, out, err := tl.checkout(t.Context(), nil, CheckoutInput{Dir: dir, Branch: "dev"})
	if err != nil {
		t.Fatal(err)
	}
	if out.Head.Branch != "dev" || out.Head.Subject != "First page" {
		t.Errorf("checked out %+v", out)
	}

	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n")
	if _, _, err := tl.checkout(t.Context(), nil, CheckoutInput{Dir: dir, Branch: "main"}); err == nil || !strings.Contains(err.Error(), "uncommitted") {
		t.Errorf("switching away from changes answered %v", err)
	}
	if _, _, err := tl.checkout(t.Context(), nil, CheckoutInput{Dir: dir, Branch: "nowhere"}); err == nil || !strings.Contains(err.Error(), "no branch nowhere") {
		t.Errorf("an unknown branch answered %v", err)
	}
}

func TestPullFastForwardsAndPushRefusesToOverwrite(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	behind := cloneSite(t, tl, url)
	diverged := cloneSite(t, tl, url)

	other := t.TempDir()
	run(t, other, "clone", "-q", bare, ".")
	write(t, filepath.Join(other, "index.html"), "<h1>Hello there</h1>\n")
	run(t, other, "commit", "-q", "-am", "Greet warmly")
	run(t, other, "push", "-q", "origin", "main")

	_, pulled, err := tl.pull(t.Context(), nil, RemoteInput{Dir: behind})
	if err != nil {
		t.Fatal(err)
	}
	if pulled.UpToDate || pulled.Head.Subject != "Greet warmly" {
		t.Errorf("pulled %+v", pulled)
	}
	if text, _ := os.ReadFile(filepath.Join(behind, "index.html")); string(text) != "<h1>Hello there</h1>\n" {
		t.Errorf("index.html after the pull: %q", text)
	}
	if _, again, err := tl.pull(t.Context(), nil, RemoteInput{Dir: behind}); err != nil || !again.UpToDate {
		t.Errorf("a second pull answered %+v, %v", again, err)
	}

	write(t, filepath.Join(diverged, "about.html"), "<h1>About</h1>\n")
	if _, _, err := tl.commit(t.Context(), nil, CommitInput{Dir: diverged, Message: "Add about", AuthorName: "Agent", AuthorEmail: "agent@example.com"}); err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: diverged}); err == nil || !strings.Contains(err.Error(), "git_pull") {
		t.Errorf("a push behind the remote answered %v", err)
	}
	if _, _, err := tl.pull(t.Context(), nil, RemoteInput{Dir: diverged}); err == nil || !strings.Contains(err.Error(), "fast-forward") {
		t.Errorf("a pull into diverged history answered %v", err)
	}
	if got := run(t, bare, "log", "-1", "--format=%s", "main"); got != "Greet warmly" {
		t.Errorf("the remote's main moved to %q", got)
	}
}

func TestForceReplacesOnlyCommitsTheCloneHasSeen(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)

	other := t.TempDir()
	run(t, other, "clone", "-q", bare, ".")
	write(t, filepath.Join(other, "index.html"), "<h1>Hello there</h1>\n")
	run(t, other, "commit", "-q", "-am", "Greet warmly")
	run(t, other, "push", "-q", "origin", "main")

	write(t, filepath.Join(dir, "index.html"), "<h1>Hi</h1>\n")
	_, committed, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Greet briefly", AuthorName: "Agent", AuthorEmail: "agent@example.com"})
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: dir, Force: true}); err == nil || !strings.Contains(err.Error(), "moved since this clone last saw it") {
		t.Errorf("forcing over a commit the clone never saw answered %v", err)
	}
	if got := run(t, bare, "log", "-1", "--format=%s", "main"); got != "Greet warmly" {
		t.Fatalf("the refused push moved the remote's main to %q", got)
	}

	if _, _, err := tl.pull(t.Context(), nil, RemoteInput{Dir: dir}); err == nil || !strings.Contains(err.Error(), "push with force") {
		t.Errorf("a pull into diverged history answered %v", err)
	}
	_, pushed, err := tl.push(t.Context(), nil, PushInput{Dir: dir, Force: true})
	if err != nil {
		t.Fatal(err)
	}
	if pushed.Commit != committed.Head.Commit {
		t.Errorf("pushed %+v", pushed)
	}
	if got := run(t, bare, "log", "--format=%s", "main"); got != "Greet briefly\nFirst page" {
		t.Errorf("the remote's main after the forced push: %q", got)
	}
}

func TestForceOnANewBranchPushesIt(t *testing.T) {
	url, bare := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	if _, _, err := tl.checkout(t.Context(), nil, CheckoutInput{Dir: dir, Branch: "fresh", Create: true}); err != nil {
		t.Fatal(err)
	}
	if _, _, err := tl.push(t.Context(), nil, PushInput{Dir: dir, Force: true}); err != nil {
		t.Fatal(err)
	}
	if got := run(t, bare, "log", "-1", "--format=%s", "fresh"); got != "First page" {
		t.Errorf("the remote's fresh is at %q", got)
	}
}

func TestLogListsCommitsNewestFirst(t *testing.T) {
	url, _ := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	write(t, filepath.Join(dir, "about.html"), "<h1>About</h1>\n")
	if _, _, err := tl.commit(t.Context(), nil, CommitInput{Dir: dir, Message: "Add about\n\nA page about us.", AuthorName: "Agent", AuthorEmail: "agent@example.com"}); err != nil {
		t.Fatal(err)
	}

	_, log, err := tl.log(t.Context(), nil, LogInput{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if len(log.Commits) != 2 || log.More || log.Commits[0].Message != "Add about\n\nA page about us." || log.Commits[0].Author != "Agent <agent@example.com>" || log.Commits[1].Message != "First page" {
		t.Errorf("log %+v", log)
	}

	_, short, err := tl.log(t.Context(), nil, LogInput{Dir: dir, Limit: 1})
	if err != nil || len(short.Commits) != 1 || !short.More {
		t.Errorf("a log of one answered %+v, %v", short, err)
	}

	_, style, err := tl.log(t.Context(), nil, LogInput{Dir: dir, Path: "style.css"})
	if err != nil || len(style.Commits) != 1 || style.Commits[0].Message != "First page" {
		t.Errorf("the log of style.css answered %+v, %v", style, err)
	}
}

func TestCloneRefusesADirectoryWithFiles(t *testing.T) {
	url, _ := remote(t)
	tl := newTools(t)
	dir := cloneSite(t, tl, url)
	if _, _, err := tl.clone(t.Context(), nil, CloneInput{URL: url, Dir: dir}); err == nil || !strings.Contains(err.Error(), "already holds files") {
		t.Errorf("cloning over a clone answered %v", err)
	}
	if _, _, err := tl.status(t.Context(), nil, DirInput{Dir: t.TempDir()}); err == nil || !strings.Contains(err.Error(), "not a git clone") {
		t.Errorf("status of a plain directory answered %v", err)
	}
}
