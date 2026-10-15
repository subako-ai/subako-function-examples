package main

import (
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/xuri/excelize/v2"
)

func sales() []Sheet {
	return []Sheet{{
		Name:    "Sales",
		Rows:    [][]any{{"item", "qty", "price", "total"}, {"apple", 3.0, 1.5, "=B2*C2"}, {"pear", 2.0, 2.25, "=B3*C3"}, {nil, nil, nil, "=SUM(D2:D3)"}},
		Header:  true,
		Widths:  []float64{12, 6},
		Formats: map[string]string{"C": "0.00"},
	}, {
		Name: "Notes",
		Rows: [][]any{{"checked", true}},
	}}
}

func TestCreateWritesTheWorkbookAndCalculatesItsFormulas(t *testing.T) {
	path := filepath.Join(t.TempDir(), "out", "sales.xlsx")
	_, created, err := create(t.Context(), nil, CreateInput{Path: path, Sheets: sales()})
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]string{"Sales!D2": "4.5", "Sales!D3": "4.5", "Sales!D4": "9"}
	if !reflect.DeepEqual(created.Calculated, want) {
		t.Errorf("calculated %v, want %v", created.Calculated, want)
	}
	wantSheets := []SheetRange{{"Sales", "A1:D4"}, {"Notes", "A1:B1"}}
	if !reflect.DeepEqual(created.Sheets, wantSheets) || created.Bytes == 0 {
		t.Errorf("created %+v", created)
	}

	f, err := excelize.OpenFile(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if width, _ := f.GetColWidth("Sales", "A"); width != 12 {
		t.Errorf("column A is %v wide", width)
	}
	panes, _ := f.GetPanes("Sales")
	if !panes.Freeze || panes.YSplit != 1 {
		t.Errorf("header row is not frozen: %+v", panes)
	}
	if price, _ := f.GetCellValue("Sales", "C2"); price != "1.50" {
		t.Errorf("C2 displays %q, not with its number format", price)
	}
}

func TestReadCalculatesFormulasExcelizeLeftUncalculated(t *testing.T) {
	path := filepath.Join(t.TempDir(), "sales.xlsx")
	if _, _, err := create(t.Context(), nil, CreateInput{Path: path, Sheets: sales()}); err != nil {
		t.Fatal(err)
	}
	_, got, err := read(t.Context(), nil, ReadInput{Path: path})
	if err != nil {
		t.Fatal(err)
	}
	want := [][]string{{"item", "qty", "price", "total"}, {"apple", "3", "1.50", "4.5"}, {"pear", "2", "2.25", "4.5"}, {"", "", "", "9"}}
	if got.Sheet != "Sales" || got.Range != "A1:D4" || !reflect.DeepEqual(got.Rows, want) {
		t.Errorf("read %s!%s as %v", got.Sheet, got.Range, got.Rows)
	}
	if got.Formulas["D4"] != "SUM(D2:D3)" {
		t.Errorf("formulas %v", got.Formulas)
	}

	_, notes, err := read(t.Context(), nil, ReadInput{Path: path, Sheet: "Notes", Range: "B1"})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(notes.Rows, [][]string{{"TRUE"}}) {
		t.Errorf("Notes!B1 read as %v", notes.Rows)
	}
}

func TestReadStopsAtMaxRows(t *testing.T) {
	rows := make([][]any, maxRows+10)
	for i := range rows {
		rows[i] = []any{float64(i)}
	}
	path := filepath.Join(t.TempDir(), "long.xlsx")
	if _, _, err := create(t.Context(), nil, CreateInput{Path: path, Sheets: []Sheet{{Name: "Long", Rows: rows}}}); err != nil {
		t.Fatal(err)
	}
	_, got, err := read(t.Context(), nil, ReadInput{Path: path})
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Rows) != maxRows || !got.Truncated {
		t.Errorf("read %d rows, truncated %v", len(got.Rows), got.Truncated)
	}
}

func TestMistakesComeBackAsErrors(t *testing.T) {
	dir := t.TempDir()
	cases := []struct {
		name string
		call func() error
		want string
	}{
		{"no sheets", func() error {
			_, _, err := create(t.Context(), nil, CreateInput{Path: filepath.Join(dir, "a.xlsx")})
			return err
		}, "at least one sheet"},
		{"a nested cell", func() error {
			sheet := Sheet{Name: "S", Rows: [][]any{{[]any{1.0}}}}
			_, _, err := create(t.Context(), nil, CreateInput{Path: filepath.Join(dir, "b.xlsx"), Sheets: []Sheet{sheet}})
			return err
		}, `sheet "S": A1: a cell holds`},
		{"a backwards range", func() error {
			path := filepath.Join(dir, "c.xlsx")
			if _, _, err := create(t.Context(), nil, CreateInput{Path: path, Sheets: sales()}); err != nil {
				return err
			}
			_, _, err := read(t.Context(), nil, ReadInput{Path: path, Range: "D4:A1"})
			return err
		}, "above and left"},
		{"a missing file", func() error {
			_, _, err := read(t.Context(), nil, ReadInput{Path: filepath.Join(dir, "missing.xlsx")})
			return err
		}, "no such file"},
	}
	for _, c := range cases {
		if err := c.call(); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%s: got %v, want an error containing %q", c.name, err, c.want)
		}
	}
}

func TestAFormulaThatFailsIsReportedNotFatal(t *testing.T) {
	sheet := Sheet{Name: "S", Rows: [][]any{{"=NOSUCHFUNCTION(1)"}}}
	_, created, err := create(t.Context(), nil, CreateInput{Path: filepath.Join(t.TempDir(), "f.xlsx"), Sheets: []Sheet{sheet}})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(created.Calculated["S!A1"], "error: ") && created.Calculated["S!A1"] != "#NAME?" {
		t.Errorf("S!A1 calculated as %q", created.Calculated["S!A1"])
	}
}
