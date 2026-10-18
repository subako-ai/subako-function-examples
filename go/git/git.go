package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/go-git/go-billy/v6/osfs"
	"github.com/go-git/go-git/v6"
	"github.com/go-git/go-git/v6/config"
	"github.com/go-git/go-git/v6/plumbing"
	"github.com/go-git/go-git/v6/plumbing/cache"
	"github.com/go-git/go-git/v6/plumbing/client"
	"github.com/go-git/go-git/v6/plumbing/filemode"
	format "github.com/go-git/go-git/v6/plumbing/format/config"
	fdiff "github.com/go-git/go-git/v6/plumbing/format/diff"
	"github.com/go-git/go-git/v6/plumbing/object"
	"github.com/go-git/go-git/v6/storage/filesystem"
	"github.com/go-git/go-git/v6/utils/diff"
	"github.com/go-git/go-git/v6/x/plugin"
	xconfig "github.com/go-git/go-git/v6/x/plugin/config"
	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/sergi/go-diff/diffmatchpatch"
)

const (
	// defaultLog and maxLog bound git_log, so a long history does not flood
	// the conversation.
	defaultLog = 20
	maxLog     = 200
	// maxDiff bounds git_diff's text, well under the 1 MiB a function's
	// answer may take.
	maxDiff = 100 * 1024
)

// A function has no system or global git config. go-git's default loader
// would read /etc/gitconfig, outside every directory WASI opens.
func init() {
	if err := plugin.Register(plugin.ConfigLoader(), func() plugin.ConfigSource { return xconfig.NewEmpty() }); err != nil {
		panic(err)
	}
}

// tools holds what every tool shares: the HTTP client git's remotes are
// reached through.
type tools struct {
	http *http.Client
}

func (t tools) clientOptions() []client.Option {
	return []client.Option{client.WithHTTPClient(t.http)}
}

type Head struct {
	// Empty on a detached HEAD.
	Branch  string `json:"branch,omitempty"`
	Commit  string `json:"commit"`
	Subject string `json:"subject"`
}

type CloneInput struct {
	URL    string `json:"url" jsonschema:"the repository's https URL, such as https://github.com/OWNER/REPO.git"`
	Dir    string `json:"dir" jsonschema:"volume directory to clone into, such as /volume/data/site; it must not exist or be empty"`
	Branch string `json:"branch,omitempty" jsonschema:"the branch to check out; the remote's default branch when empty"`
	Depth  int    `json:"depth,omitempty" jsonschema:"fetch only this many recent commits of the one branch, as git clone --depth does, which makes a large repository quicker to clone; every branch's whole history when empty"`
}

type Cloned struct {
	Dir   string `json:"dir"`
	Head  Head   `json:"head"`
	Files int    `json:"files" jsonschema:"files in the checked-out tree"`
}

func (t tools) clone(ctx context.Context, _ *mcp.CallToolRequest, in CloneInput) (*mcp.CallToolResult, Cloned, error) {
	entries, err := os.ReadDir(in.Dir)
	existed := err == nil
	if len(entries) > 0 {
		return nil, Cloned{}, fmt.Errorf("%s already holds files; clone into a new directory, or use git_pull to update a clone", in.Dir)
	}
	opts := &git.CloneOptions{URL: in.URL, Depth: in.Depth, SingleBranch: in.Depth > 0, ClientOptions: t.clientOptions()}
	if in.Branch != "" {
		opts.ReferenceName = plumbing.NewBranchReferenceName(in.Branch)
	}
	r, err := cloneInto(ctx, in.Dir, opts)
	if err != nil {
		// Leave dir as it was: empty, or not there.
		if existed {
			entries, _ := os.ReadDir(in.Dir)
			for _, e := range entries {
				os.RemoveAll(filepath.Join(in.Dir, e.Name()))
			}
		} else {
			os.RemoveAll(in.Dir)
		}
		return nil, Cloned{}, fmt.Errorf("clone %s: %w", in.URL, err)
	}
	head, err := headOf(r)
	if err != nil {
		return nil, Cloned{}, err
	}
	files, err := countFiles(r)
	if err != nil {
		return nil, Cloned{}, err
	}
	return nil, Cloned{Dir: in.Dir, Head: head, Files: files}, nil
}

type DirInput struct {
	Dir string `json:"dir" jsonschema:"the clone's directory on the volume"`
}

type Change struct {
	Path   string `json:"path"`
	Change string `json:"change" jsonschema:"added, modified, deleted, renamed or copied"`
}

type Status struct {
	Head     Head     `json:"head"`
	Branches []string `json:"branches" jsonschema:"the local branches"`
	Changes  []Change `json:"changes" jsonschema:"files that differ from the last commit"`
}

func (t tools) status(_ context.Context, _ *mcp.CallToolRequest, in DirInput) (*mcp.CallToolResult, Status, error) {
	r, w, err := open(in.Dir)
	if err != nil {
		return nil, Status{}, err
	}
	head, err := headOf(r)
	if err != nil {
		return nil, Status{}, err
	}
	branches, err := localBranches(r)
	if err != nil {
		return nil, Status{}, err
	}
	changes, err := changesOf(w)
	if err != nil {
		return nil, Status{}, err
	}
	return nil, Status{Head: head, Branches: branches, Changes: changes}, nil
}

type DiffInput struct {
	Dir   string   `json:"dir" jsonschema:"the clone's directory on the volume"`
	Paths []string `json:"paths,omitempty" jsonschema:"files to diff, relative to dir; every changed file when empty"`
}

type Diff struct {
	Changes []Change `json:"changes"`
	Diff    string   `json:"diff" jsonschema:"a unified diff from the last commit to the files on the volume"`
	Cut     bool     `json:"cut,omitempty" jsonschema:"the diff was longer and ends early; pass fewer paths for the rest"`
}

func (t tools) diff(_ context.Context, _ *mcp.CallToolRequest, in DiffInput) (*mcp.CallToolResult, Diff, error) {
	r, w, err := open(in.Dir)
	if err != nil {
		return nil, Diff{}, err
	}
	changes, err := changesOf(w)
	if err != nil {
		return nil, Diff{}, err
	}
	if len(in.Paths) > 0 {
		changes = slices.DeleteFunc(changes, func(c Change) bool { return !slices.Contains(in.Paths, c.Path) })
	}
	tree, err := headTree(r)
	if err != nil {
		return nil, Diff{}, err
	}
	patches := make([]fdiff.FilePatch, 0, len(changes))
	for _, c := range changes {
		p, err := worktreePatch(in.Dir, tree, c.Path)
		if err != nil {
			return nil, Diff{}, fmt.Errorf("%s: %w", c.Path, err)
		}
		patches = append(patches, p)
	}
	var out bytes.Buffer
	if err := fdiff.NewUnifiedEncoder(&out, fdiff.DefaultContextLines).Encode(patch(patches)); err != nil {
		return nil, Diff{}, err
	}
	text, cut := cutText(out.String(), maxDiff)
	return nil, Diff{Changes: changes, Diff: text, Cut: cut}, nil
}

type CommitInput struct {
	Dir         string   `json:"dir" jsonschema:"the clone's directory on the volume"`
	Message     string   `json:"message" jsonschema:"the commit message"`
	Paths       []string `json:"paths,omitempty" jsonschema:"files to commit, relative to dir; every changed file, new and deleted ones included, when empty"`
	AuthorName  string   `json:"author_name,omitempty" jsonschema:"the name of the person the commit is for, exactly as they gave it; never made up or guessed. When you do not know it, ask them before committing. The clone's user.name when empty"`
	AuthorEmail string   `json:"author_email,omitempty" jsonschema:"that person's email, exactly as they gave it; never made up or guessed. When you do not know it, ask them before committing. The clone's user.email when empty"`
}

type Committed struct {
	Head    Head     `json:"head"`
	Changes []Change `json:"changes" jsonschema:"what the commit changed"`
}

func (t tools) commit(_ context.Context, _ *mcp.CallToolRequest, in CommitInput) (*mcp.CallToolResult, Committed, error) {
	if strings.TrimSpace(in.Message) == "" {
		return nil, Committed{}, errors.New("a commit needs a message")
	}
	r, w, err := open(in.Dir)
	if err != nil {
		return nil, Committed{}, err
	}
	author, err := authorOf(r, in.AuthorName, in.AuthorEmail)
	if err != nil {
		return nil, Committed{}, err
	}
	if err := stage(w, in.Paths); err != nil {
		return nil, Committed{}, err
	}
	if err := keepExecutable(r); err != nil {
		return nil, Committed{}, err
	}
	staged, err := stagedChanges(w)
	if err != nil {
		return nil, Committed{}, err
	}
	if len(staged) == 0 {
		return nil, Committed{}, errors.New("nothing to commit: no file differs from the last commit")
	}
	if _, err := w.Commit(in.Message, &git.CommitOptions{Author: author}); err != nil {
		return nil, Committed{}, err
	}
	head, err := headOf(r)
	if err != nil {
		return nil, Committed{}, err
	}
	return nil, Committed{Head: head, Changes: staged}, nil
}

type RemoteInput struct {
	Dir    string `json:"dir" jsonschema:"the clone's directory on the volume"`
	Remote string `json:"remote,omitempty" jsonschema:"the remote's name; origin when empty"`
}

type PushInput struct {
	Dir    string `json:"dir" jsonschema:"the clone's directory on the volume"`
	Remote string `json:"remote,omitempty" jsonschema:"the remote's name; origin when empty"`
	Force  bool   `json:"force,omitempty" jsonschema:"replace the remote branch even where it has commits this clone lacks, as git push --force-with-lease does: only while the remote branch is still where this clone last saw it, so git_pull first to see commits pushed since"`
}

type Pushed struct {
	Remote   string `json:"remote" jsonschema:"the remote's URL"`
	Branch   string `json:"branch"`
	Commit   string `json:"commit"`
	UpToDate bool   `json:"up_to_date,omitempty" jsonschema:"the remote already had the commit, so nothing was sent"`
}

func (t tools) push(ctx context.Context, _ *mcp.CallToolRequest, in PushInput) (*mcp.CallToolResult, Pushed, error) {
	r, _, err := open(in.Dir)
	if err != nil {
		return nil, Pushed{}, err
	}
	head, err := headOf(r)
	if err != nil {
		return nil, Pushed{}, err
	}
	if head.Branch == "" {
		return nil, Pushed{}, errors.New("HEAD is not on a branch; switch to one with git_checkout first")
	}
	remote, url, err := remoteOf(r, in.Remote)
	if err != nil {
		return nil, Pushed{}, err
	}
	ref := plumbing.NewBranchReferenceName(head.Branch)
	opts := &git.PushOptions{
		RemoteName:    remote,
		RefSpecs:      []config.RefSpec{config.RefSpec(ref + ":" + ref)},
		ClientOptions: t.clientOptions(),
	}
	// The lease is the remote-tracking branch, which a branch never fetched
	// lacks; the remote cannot have commits this clone saw on it, so such a
	// push stays an ordinary one.
	leased := false
	if in.Force {
		_, err := r.Reference(plumbing.NewRemoteReferenceName(remote, head.Branch), true)
		switch {
		case err == nil:
			opts.ForceWithLease = &git.ForceWithLease{}
			leased = true
		case !errors.Is(err, plumbing.ErrReferenceNotFound):
			return nil, Pushed{}, err
		}
	}
	err = r.PushContext(ctx, opts)
	pushed := Pushed{Remote: url, Branch: head.Branch, Commit: head.Commit}
	switch {
	case err == nil:
	case errors.Is(err, git.NoErrAlreadyUpToDate):
		pushed.UpToDate = true
	// go-git reports a push behind the remote, and a broken lease, with a
	// bare fmt error, not ErrNonFastForwardUpdate, so only its text tells.
	case strings.Contains(err.Error(), "non-fast-forward update") && leased:
		return nil, Pushed{}, fmt.Errorf("%s on %s has moved since this clone last saw it, so force would overwrite commits no one here has seen; git_pull to see them, then push with force again if they should still go", head.Branch, url)
	case strings.Contains(err.Error(), "non-fast-forward update"):
		return nil, Pushed{}, fmt.Errorf("%s has commits on %s that this clone lacks; git_pull them first, or push with force to replace them", url, head.Branch)
	default:
		return nil, Pushed{}, fmt.Errorf("push to %s: %w", url, err)
	}
	return nil, pushed, nil
}

type Pulled struct {
	Head     Head `json:"head"`
	UpToDate bool `json:"up_to_date,omitempty" jsonschema:"the remote had nothing new"`
}

func (t tools) pull(ctx context.Context, _ *mcp.CallToolRequest, in RemoteInput) (*mcp.CallToolResult, Pulled, error) {
	r, w, err := open(in.Dir)
	if err != nil {
		return nil, Pulled{}, err
	}
	head, err := headOf(r)
	if err != nil {
		return nil, Pulled{}, err
	}
	if head.Branch == "" {
		return nil, Pulled{}, errors.New("HEAD is not on a branch; switch to one with git_checkout first")
	}
	remote, url, err := remoteOf(r, in.Remote)
	if err != nil {
		return nil, Pulled{}, err
	}
	err = w.PullContext(ctx, &git.PullOptions{
		RemoteName:    remote,
		ReferenceName: plumbing.NewBranchReferenceName(head.Branch),
		ClientOptions: t.clientOptions(),
	})
	switch {
	case errors.Is(err, git.NoErrAlreadyUpToDate):
		return nil, Pulled{Head: head, UpToDate: true}, nil
	case errors.Is(err, git.ErrNonFastForwardUpdate):
		return nil, Pulled{}, fmt.Errorf("%s on %s and this clone have each moved on, and only a fast-forward pull is supported; push this clone's commits to a new branch instead, or push with force to replace the remote's", head.Branch, url)
	case errors.Is(err, git.ErrUnstagedChanges), errors.Is(err, git.ErrWorktreeNotClean):
		return nil, Pulled{}, errors.New("the clone has uncommitted changes; commit them with git_commit before pulling")
	case err != nil:
		return nil, Pulled{}, fmt.Errorf("pull from %s: %w", url, err)
	}
	if err := keepExecutable(r); err != nil {
		return nil, Pulled{}, err
	}
	head, err = headOf(r)
	if err != nil {
		return nil, Pulled{}, err
	}
	return nil, Pulled{Head: head}, nil
}

type LogInput struct {
	Dir   string `json:"dir" jsonschema:"the clone's directory on the volume"`
	Limit int    `json:"limit,omitempty" jsonschema:"how many commits to return, newest first; 20 when empty, at most 200"`
	Path  string `json:"path,omitempty" jsonschema:"only commits that change this file, relative to dir"`
}

type Commit struct {
	Commit  string    `json:"commit"`
	Author  string    `json:"author"`
	Date    time.Time `json:"date"`
	Message string    `json:"message"`
}

type Log struct {
	Commits []Commit `json:"commits"`
	More    bool     `json:"more,omitempty" jsonschema:"older commits follow; raise limit to see them"`
}

func (t tools) log(_ context.Context, _ *mcp.CallToolRequest, in LogInput) (*mcp.CallToolResult, Log, error) {
	limit := in.Limit
	switch {
	case limit <= 0:
		limit = defaultLog
	case limit > maxLog:
		limit = maxLog
	}
	r, _, err := open(in.Dir)
	if err != nil {
		return nil, Log{}, err
	}
	opts := &git.LogOptions{}
	if in.Path != "" {
		opts.FileName = &in.Path
	}
	iter, err := r.Log(opts)
	if err != nil {
		return nil, Log{}, err
	}
	defer iter.Close()
	var log Log
	shallow, err := r.Storer.Shallow()
	if err != nil {
		return nil, Log{}, err
	}
	for {
		c, err := iter.Next()
		// A shallow clone's history ends at commits whose parents it lacks.
		if errors.Is(err, io.EOF) || (len(shallow) > 0 && errors.Is(err, plumbing.ErrObjectNotFound)) {
			break
		}
		if err != nil {
			return nil, Log{}, err
		}
		if len(log.Commits) == limit {
			log.More = true
			break
		}
		log.Commits = append(log.Commits, Commit{
			Commit:  c.Hash.String(),
			Author:  fmt.Sprintf("%s <%s>", c.Author.Name, c.Author.Email),
			Date:    c.Author.When,
			Message: strings.TrimSpace(c.Message),
		})
	}
	return nil, log, nil
}

type CheckoutInput struct {
	Dir    string `json:"dir" jsonschema:"the clone's directory on the volume"`
	Branch string `json:"branch" jsonschema:"the branch to switch to"`
	Create bool   `json:"create,omitempty" jsonschema:"create the branch at the current commit, keeping uncommitted changes, as git switch -c does"`
}

type CheckedOut struct {
	Head Head `json:"head"`
}

func (t tools) checkout(_ context.Context, _ *mcp.CallToolRequest, in CheckoutInput) (*mcp.CallToolResult, CheckedOut, error) {
	r, w, err := open(in.Dir)
	if err != nil {
		return nil, CheckedOut{}, err
	}
	ref := plumbing.NewBranchReferenceName(in.Branch)
	opts := &git.CheckoutOptions{Branch: ref}
	switch _, err := r.Reference(ref, false); {
	case in.Create:
		opts.Create = true
		opts.Keep = true
	case errors.Is(err, plumbing.ErrReferenceNotFound):
		// A branch only the remote has, as git switch does with its guess.
		remote, rerr := r.Reference(plumbing.NewRemoteReferenceName(git.DefaultRemoteName, in.Branch), true)
		if rerr != nil {
			return nil, CheckedOut{}, fmt.Errorf("no branch %s here or on %s; pass create to make one", in.Branch, git.DefaultRemoteName)
		}
		opts.Create = true
		opts.Hash = remote.Hash()
	case err != nil:
		return nil, CheckedOut{}, err
	}
	if !in.Create {
		changes, err := changesOf(w)
		if err != nil {
			return nil, CheckedOut{}, err
		}
		if len(changes) > 0 {
			return nil, CheckedOut{}, errors.New("the clone has uncommitted changes; commit them with git_commit, or pass create to take them to a new branch")
		}
	}
	if err := w.Checkout(opts); err != nil {
		return nil, CheckedOut{}, err
	}
	if err := keepExecutable(r); err != nil {
		return nil, CheckedOut{}, err
	}
	head, err := headOf(r)
	if err != nil {
		return nil, CheckedOut{}, err
	}
	return nil, CheckedOut{Head: head}, nil
}

// cloneInto is git.PlainCloneContext without its check that dir is empty,
// which stats through a filesystem rooted at /, a directory WASI does not
// open.
func cloneInto(ctx context.Context, dir string, opts *git.CloneOptions) (*git.Repository, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	wt := osfs.New(dir, osfs.WithBoundOS())
	dot, err := wt.Chroot(git.GitDirName)
	if err != nil {
		return nil, err
	}
	r, err := git.CloneContext(ctx, filesystem.NewStorage(dot, cache.NewObjectLRUDefault()), wt, opts)
	if err != nil {
		return nil, err
	}
	// WASI has no executable bit. As git does on such a filesystem, the clone
	// then reads its absence as no change.
	keeps, err := keepsExecutableBit(filepath.Join(dir, git.GitDirName))
	if err != nil || keeps {
		return r, err
	}
	cfg, err := r.Config()
	if err != nil {
		return nil, err
	}
	cfg.Core.FileMode = false
	if err := r.SetConfig(cfg); err != nil {
		return nil, err
	}
	return r, keepExecutable(r)
}

// keepsExecutableBit reports whether files in dir keep an executable bit, as
// git checks when it makes a repository.
func keepsExecutableBit(dir string) (bool, error) {
	f, err := os.CreateTemp(dir, "filemode-probe")
	if err != nil {
		return false, err
	}
	name := f.Name()
	f.Close()
	defer os.Remove(name)
	if err := os.Chmod(name, 0o755); err != nil {
		return false, nil
	}
	info, err := os.Stat(name)
	if err != nil {
		return false, err
	}
	return info.Mode()&0o100 != 0, nil
}

// keepExecutable restores the executable mode HEAD gives an index entry when
// core.fileMode is off. go-git takes an entry's mode from the filesystem
// whatever core.fileMode says, so every checkout and add would drop the bit.
func keepExecutable(r *git.Repository) error {
	cfg, err := r.Config()
	if err != nil || cfg.Core.FileMode {
		return err
	}
	tree, err := headTree(r)
	if err != nil {
		return err
	}
	idx, err := r.Storer.Index()
	if err != nil {
		return err
	}
	changed := false
	for _, e := range idx.Entries {
		if e.Mode != filemode.Regular {
			continue
		}
		if f, err := tree.File(e.Name); err == nil && f.Mode == filemode.Executable {
			e.Mode = filemode.Executable
			changed = true
		}
	}
	if !changed {
		return nil
	}
	return r.Storer.SetIndex(idx)
}

func open(dir string) (*git.Repository, *git.Worktree, error) {
	r, err := git.PlainOpen(dir)
	if errors.Is(err, git.ErrRepositoryNotExists) {
		return nil, nil, fmt.Errorf("%s is not a git clone; make one with git_clone", dir)
	}
	if err != nil {
		return nil, nil, err
	}
	w, err := r.Worktree()
	if err != nil {
		return nil, nil, err
	}
	return r, w, nil
}

func headOf(r *git.Repository) (Head, error) {
	ref, err := r.Head()
	if err != nil {
		return Head{}, err
	}
	c, err := r.CommitObject(ref.Hash())
	if err != nil {
		return Head{}, err
	}
	head := Head{Commit: c.Hash.String(), Subject: subject(c.Message)}
	if ref.Name().IsBranch() {
		head.Branch = ref.Name().Short()
	}
	return head, nil
}

func subject(message string) string {
	line, _, _ := strings.Cut(strings.TrimSpace(message), "\n")
	return line
}

func headTree(r *git.Repository) (*object.Tree, error) {
	ref, err := r.Head()
	if err != nil {
		return nil, err
	}
	c, err := r.CommitObject(ref.Hash())
	if err != nil {
		return nil, err
	}
	return c.Tree()
}

func countFiles(r *git.Repository) (int, error) {
	tree, err := headTree(r)
	if err != nil {
		return 0, err
	}
	n := 0
	err = tree.Files().ForEach(func(*object.File) error { n++; return nil })
	return n, err
}

func localBranches(r *git.Repository) ([]string, error) {
	iter, err := r.Branches()
	if err != nil {
		return nil, err
	}
	branches := []string{}
	err = iter.ForEach(func(ref *plumbing.Reference) error {
		branches = append(branches, ref.Name().Short())
		return nil
	})
	slices.Sort(branches)
	return branches, err
}

func remoteOf(r *git.Repository, name string) (string, string, error) {
	if name == "" {
		name = git.DefaultRemoteName
	}
	remote, err := r.Remote(name)
	if err != nil {
		return "", "", fmt.Errorf("remote %s: %w", name, err)
	}
	return name, remote.Config().URLs[0], nil
}

// changesOf lists the files that differ from HEAD, staged or not, sorted by
// path.
func changesOf(w *git.Worktree) ([]Change, error) {
	st, err := w.Status()
	if err != nil {
		return nil, err
	}
	changes := []Change{}
	for path, fs := range st {
		code := fs.Staging
		if code == git.Unmodified || code == git.Untracked {
			code = fs.Worktree
		}
		if change, ok := changeName(code); ok {
			changes = append(changes, Change{Path: path, Change: change})
		}
	}
	slices.SortFunc(changes, func(a, b Change) int { return strings.Compare(a.Path, b.Path) })
	return changes, nil
}

func stagedChanges(w *git.Worktree) ([]Change, error) {
	st, err := w.Status()
	if err != nil {
		return nil, err
	}
	changes := []Change{}
	for path, fs := range st {
		if change, ok := changeName(fs.Staging); ok && fs.Staging != git.Untracked {
			changes = append(changes, Change{Path: path, Change: change})
		}
	}
	slices.SortFunc(changes, func(a, b Change) int { return strings.Compare(a.Path, b.Path) })
	return changes, nil
}

func changeName(code git.StatusCode) (string, bool) {
	switch code {
	case git.Untracked, git.Added:
		return "added", true
	case git.Modified:
		return "modified", true
	case git.Deleted:
		return "deleted", true
	case git.Renamed:
		return "renamed", true
	case git.Copied:
		return "copied", true
	case git.Unmodified, git.UpdatedButUnmerged:
		return "", false
	default:
		return "", false
	}
}

func stage(w *git.Worktree, paths []string) error {
	if len(paths) == 0 {
		return w.AddWithOptions(&git.AddOptions{All: true})
	}
	for _, path := range paths {
		var err error
		if _, statErr := w.Filesystem().Lstat(path); errors.Is(statErr, os.ErrNotExist) {
			_, err = w.Remove(path)
		} else {
			_, err = w.Add(path)
		}
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
	}
	return nil
}

// authorOf is the commit's author: the input's, else the clone's own
// user.name and user.email. A function has no global git config to fall back
// on.
func authorOf(r *git.Repository, name, email string) (*object.Signature, error) {
	if name == "" || email == "" {
		cfg, err := r.Config()
		if err != nil {
			return nil, err
		}
		if name == "" {
			name = cfg.User.Name
		}
		if email == "" {
			email = cfg.User.Email
		}
	}
	if name == "" || email == "" {
		return nil, errors.New("a commit needs an author: ask the user for the name and email to commit as, then pass them as author_name and author_email; do not make them up")
	}
	return &object.Signature{Name: name, Email: email, When: time.Now()}, nil
}

// worktreePatch diffs the file at path from HEAD's tree to the volume.
func worktreePatch(dir string, tree *object.Tree, path string) (fdiff.FilePatch, error) {
	var from *file
	var fromText string
	if f, err := tree.File(path); err == nil {
		text, err := f.Contents()
		if err != nil {
			return nil, err
		}
		from = &file{hash: f.Hash, mode: f.Mode, path: path}
		fromText = text
	} else if !errors.Is(err, object.ErrFileNotFound) {
		return nil, err
	}

	var to *file
	var toText string
	data, err := os.ReadFile(filepath.Join(dir, filepath.FromSlash(path)))
	switch {
	case err == nil:
		mode := filemode.Regular
		if from != nil {
			mode = from.mode
		}
		to = &file{hash: blobHash(data), mode: mode, path: path}
		toText = string(data)
	case !errors.Is(err, os.ErrNotExist):
		return nil, err
	}

	p := filePatch{from: from, to: to}
	if binary(fromText) || binary(toText) {
		p.binary = true
		return p, nil
	}
	for _, d := range diff.Do(fromText, toText) {
		p.chunks = append(p.chunks, chunk{text: d.Text, op: operation(d.Type)})
	}
	return p, nil
}

func blobHash(data []byte) plumbing.Hash {
	h := plumbing.NewHasher(format.SHA1, plumbing.BlobObject, int64(len(data)))
	h.Write(data)
	return h.Sum()
}

// binary is git's own test: a NUL in the first 8000 bytes.
func binary(text string) bool {
	return strings.ContainsRune(text[:min(len(text), 8000)], 0)
}

func operation(t diffmatchpatch.Operation) fdiff.Operation {
	switch t {
	case diffmatchpatch.DiffInsert:
		return fdiff.Add
	case diffmatchpatch.DiffDelete:
		return fdiff.Delete
	case diffmatchpatch.DiffEqual:
		return fdiff.Equal
	default:
		return fdiff.Equal
	}
}

// cutText keeps text under max bytes, ending at a line.
func cutText(text string, max int) (string, bool) {
	if len(text) <= max {
		return text, false
	}
	end := strings.LastIndexByte(text[:max], '\n') + 1
	if end == 0 {
		end = max
	}
	return text[:end], true
}

type patch []fdiff.FilePatch

func (p patch) FilePatches() []fdiff.FilePatch { return p }
func (p patch) Message() string                { return "" }

type filePatch struct {
	from, to *file
	binary   bool
	chunks   []fdiff.Chunk
}

func (p filePatch) IsBinary() bool        { return p.binary }
func (p filePatch) Chunks() []fdiff.Chunk { return p.chunks }

// Files returns untyped nils for a side that does not exist, as the encoder
// checks with == nil.
func (p filePatch) Files() (fdiff.File, fdiff.File) {
	var from, to fdiff.File
	if p.from != nil {
		from = p.from
	}
	if p.to != nil {
		to = p.to
	}
	return from, to
}

type file struct {
	hash plumbing.Hash
	mode filemode.FileMode
	path string
}

func (f *file) Hash() plumbing.Hash     { return f.hash }
func (f *file) Mode() filemode.FileMode { return f.mode }
func (f *file) Path() string            { return f.path }

type chunk struct {
	text string
	op   fdiff.Operation
}

func (c chunk) Content() string       { return c.text }
func (c chunk) Type() fdiff.Operation { return c.op }
