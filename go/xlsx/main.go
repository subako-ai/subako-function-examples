// A Subako function package that writes and reads Excel workbooks on the
// session volume with excelize.
package main

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"strings"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	subakofn "github.com/subako-ai/subako-function-sdk-go"
	"github.com/xuri/excelize/v2"
)

// maxRows bounds what read_xlsx returns, so a large sheet does not flood the
// conversation; the caller narrows the range to read past it.
const maxRows = 500

type Sheet struct {
	Name    string            `json:"name" jsonschema:"the sheet's name, at most 31 characters"`
	Rows    [][]any           `json:"rows" jsonschema:"rows of cells from A1: numbers, booleans, strings, or null for an empty cell; a string starting with = is a formula, such as =SUM(B2:B9)"`
	Header  bool              `json:"header,omitempty" jsonschema:"bold the first row and keep it in view while scrolling"`
	Widths  []float64         `json:"column_widths,omitempty" jsonschema:"widths of columns A, B, ... in characters"`
	Formats map[string]string `json:"number_formats,omitempty" jsonschema:"number formats by column letter, such as {\"C\": \"#,##0.00\"}; the header row keeps its own"`
}

type CreateInput struct {
	Path   string  `json:"path" jsonschema:"volume path to write the .xlsx to, such as /volume/tmp/report.xlsx; missing directories are created"`
	Sheets []Sheet `json:"sheets" jsonschema:"the workbook's sheets, in order"`
}

type SheetRange struct {
	Name  string `json:"name"`
	Range string `json:"range" jsonschema:"the sheet's used range, such as A1:D12"`
}

type Created struct {
	Path   string       `json:"path"`
	Bytes  int64        `json:"bytes"`
	Sheets []SheetRange `json:"sheets"`
	// What each formula came to, so a mistake shows up before anyone opens
	// the file.
	Calculated map[string]string `json:"calculated,omitempty" jsonschema:"each formula's value by sheet!cell, or the error calculating it"`
}

func create(ctx context.Context, req *mcp.CallToolRequest, in CreateInput) (*mcp.CallToolResult, Created, error) {
	if len(in.Sheets) == 0 {
		return nil, Created{}, errors.New("a workbook needs at least one sheet")
	}
	f := excelize.NewFile()
	defer f.Close()
	calculated := map[string]string{}
	for i, sheet := range in.Sheets {
		if err := addSheet(f, i, sheet); err != nil {
			return nil, Created{}, fmt.Errorf("sheet %q: %w", sheet.Name, err)
		}
		for cell := range formulas(sheet) {
			value, err := f.CalcCellValue(sheet.Name, cell)
			if err != nil {
				value = "error: " + err.Error()
			}
			calculated[sheet.Name+"!"+cell] = value
		}
	}
	if err := os.MkdirAll(filepath.Dir(in.Path), 0o755); err != nil {
		return nil, Created{}, err
	}
	if err := f.SaveAs(in.Path); err != nil {
		return nil, Created{}, err
	}
	info, err := os.Stat(in.Path)
	if err != nil {
		return nil, Created{}, err
	}
	sheets, err := ranges(f)
	if err != nil {
		return nil, Created{}, err
	}
	return nil, Created{Path: in.Path, Bytes: info.Size(), Sheets: sheets, Calculated: calculated}, nil
}

// addSheet writes sheet as the workbook's i-th sheet; the first takes the
// place of the blank sheet a new file starts with.
func addSheet(f *excelize.File, i int, sheet Sheet) error {
	if i == 0 {
		if err := f.SetSheetName(f.GetSheetName(0), sheet.Name); err != nil {
			return err
		}
	} else if _, err := f.NewSheet(sheet.Name); err != nil {
		return err
	}
	for r, row := range sheet.Rows {
		for c, value := range row {
			cell, err := excelize.CoordinatesToCellName(c+1, r+1)
			if err != nil {
				return err
			}
			if err := setCell(f, sheet.Name, cell, value); err != nil {
				return fmt.Errorf("%s: %w", cell, err)
			}
		}
	}
	for c, width := range sheet.Widths {
		column, err := excelize.ColumnNumberToName(c + 1)
		if err != nil {
			return err
		}
		if err := f.SetColWidth(sheet.Name, column, column, width); err != nil {
			return err
		}
	}
	for column, format := range sheet.Formats {
		style, err := f.NewStyle(&excelize.Style{CustomNumFmt: &format})
		if err != nil {
			return fmt.Errorf("number format %q: %w", format, err)
		}
		if err := f.SetColStyle(sheet.Name, column, style); err != nil {
			return err
		}
	}
	if sheet.Header && len(sheet.Rows) > 0 {
		bold, err := f.NewStyle(&excelize.Style{Font: &excelize.Font{Bold: true}})
		if err != nil {
			return err
		}
		last, err := excelize.CoordinatesToCellName(max(len(sheet.Rows[0]), 1), 1)
		if err != nil {
			return err
		}
		if err := f.SetCellStyle(sheet.Name, "A1", last, bold); err != nil {
			return err
		}
		pane := excelize.Panes{Freeze: true, YSplit: 1, TopLeftCell: "A2", ActivePane: "bottomLeft"}
		if err := f.SetPanes(sheet.Name, &pane); err != nil {
			return err
		}
	}
	// excelize leaves the stored used range at A1 as cells are written.
	width := 1
	for _, row := range sheet.Rows {
		width = max(width, len(row))
	}
	last, err := excelize.CoordinatesToCellName(width, max(len(sheet.Rows), 1))
	if err != nil {
		return err
	}
	return f.SetSheetDimension(sheet.Name, "A1:"+last)
}

func setCell(f *excelize.File, sheet, cell string, value any) error {
	switch v := value.(type) {
	case nil:
		return nil
	case string:
		if formula, ok := strings.CutPrefix(v, "="); ok {
			return f.SetCellFormula(sheet, cell, formula)
		}
		return f.SetCellStr(sheet, cell, v)
	case float64, bool:
		return f.SetCellValue(sheet, cell, v)
	default:
		return fmt.Errorf("a cell holds a number, a boolean, a string or null, not %T", value)
	}
}

// formulas yields the cells in sheet that hold a formula.
func formulas(sheet Sheet) iter.Seq[string] {
	return func(yield func(string) bool) {
		for r, row := range sheet.Rows {
			for c, value := range row {
				s, ok := value.(string)
				if !ok || !strings.HasPrefix(s, "=") {
					continue
				}
				cell, err := excelize.CoordinatesToCellName(c+1, r+1)
				if err == nil && !yield(cell) {
					return
				}
			}
		}
	}
}

func ranges(f *excelize.File) ([]SheetRange, error) {
	sheets := []SheetRange{}
	for _, name := range f.GetSheetList() {
		used, err := usedRange(f, name)
		if err != nil {
			return nil, err
		}
		sheets = append(sheets, SheetRange{Name: name, Range: used})
	}
	return sheets, nil
}

// usedRange is the range a sheet records, widened to the cells that hold
// values, since not every program that writes a workbook keeps the record up
// to date.
func usedRange(f *excelize.File, sheet string) (string, error) {
	recorded, err := f.GetSheetDimension(sheet)
	if err != nil {
		return "", err
	}
	rows, err := f.GetRows(sheet)
	if err != nil {
		return "", err
	}
	right, bottom := 1, max(len(rows), 1)
	if _, _, r, b, err := corners(recorded); err == nil {
		right, bottom = max(right, r), max(bottom, b)
	}
	for _, row := range rows {
		right = max(right, len(row))
	}
	last, err := excelize.CoordinatesToCellName(right, bottom)
	if err != nil {
		return "", err
	}
	return "A1:" + last, nil
}

type ReadInput struct {
	Path  string `json:"path" jsonschema:"volume path of the .xlsx to read"`
	Sheet string `json:"sheet,omitempty" jsonschema:"the sheet to read; the first sheet when omitted"`
	Range string `json:"range,omitempty" jsonschema:"the cells to read, such as A1:F20; the sheet's used range when omitted"`
}

type Read struct {
	Sheets    []SheetRange      `json:"sheets" jsonschema:"every sheet in the workbook and its used range"`
	Sheet     string            `json:"sheet"`
	Range     string            `json:"range" jsonschema:"the cells in rows"`
	Rows      [][]string        `json:"rows" jsonschema:"cell values as Excel displays them, with formulas calculated"`
	Formulas  map[string]string `json:"formulas,omitempty" jsonschema:"the formula in each cell that has one"`
	Truncated bool              `json:"truncated,omitempty" jsonschema:"whether rows stop short of the range; read the rest with a narrower range"`
}

func read(ctx context.Context, req *mcp.CallToolRequest, in ReadInput) (*mcp.CallToolResult, Read, error) {
	f, err := excelize.OpenFile(in.Path)
	if err != nil {
		return nil, Read{}, err
	}
	defer f.Close()
	sheets, err := ranges(f)
	if err != nil {
		return nil, Read{}, err
	}
	sheet := in.Sheet
	if sheet == "" {
		sheet = sheets[0].Name
	}
	area := in.Range
	if area == "" {
		if area, err = usedRange(f, sheet); err != nil {
			return nil, Read{}, err
		}
	}
	left, top, right, bottom, err := corners(area)
	if err != nil {
		return nil, Read{}, fmt.Errorf("range %q: %w", area, err)
	}
	out := Read{Sheets: sheets, Sheet: sheet, Range: area, Rows: [][]string{}, Formulas: map[string]string{}}
	for r := top; r <= bottom; r++ {
		if len(out.Rows) == maxRows {
			out.Truncated = true
			break
		}
		row := make([]string, 0, right-left+1)
		for c := left; c <= right; c++ {
			cell, err := excelize.CoordinatesToCellName(c, r)
			if err != nil {
				return nil, Read{}, err
			}
			value, formula, err := cellValue(f, sheet, cell)
			if err != nil {
				return nil, Read{}, fmt.Errorf("%s!%s: %w", sheet, cell, err)
			}
			if formula != "" {
				out.Formulas[cell] = formula
			}
			row = append(row, value)
		}
		out.Rows = append(out.Rows, row)
	}
	return nil, out, nil
}

// cellValue is what a cell displays and its formula, if any. A formula
// without a stored result, as in a workbook excelize wrote, is calculated.
func cellValue(f *excelize.File, sheet, cell string) (value, formula string, err error) {
	if value, err = f.GetCellValue(sheet, cell); err != nil {
		return "", "", err
	}
	if formula, err = f.GetCellFormula(sheet, cell); err != nil || formula == "" || value != "" {
		return value, formula, err
	}
	calculated, err := f.CalcCellValue(sheet, cell)
	if err != nil {
		return "error: " + err.Error(), formula, nil
	}
	return calculated, formula, nil
}

// corners parses a range such as A1:F20, or a single cell, into its columns
// and rows.
func corners(area string) (left, top, right, bottom int, err error) {
	from, to, ok := strings.Cut(area, ":")
	if !ok {
		to = from
	}
	if left, top, err = excelize.CellNameToCoordinates(from); err != nil {
		return
	}
	if right, bottom, err = excelize.CellNameToCoordinates(to); err != nil {
		return
	}
	if right < left || bottom < top {
		err = errors.New("the first cell must be above and left of the second")
	}
	return
}

func main() {
	server := mcp.NewServer(&mcp.Implementation{Name: "xlsx", Version: "0.1.0"}, nil)
	mcp.AddTool(server, &mcp.Tool{Name: "create_xlsx", Description: "Write an Excel workbook to the volume from sheets of rows, with formulas, a bold frozen header row, column widths and number formats. Returns what each formula calculates to."}, create)
	mcp.AddTool(server, &mcp.Tool{Name: "read_xlsx", Description: "Read cells from an Excel workbook on the volume as Excel displays them, with formulas calculated, along with every sheet's name and used range."}, read)
	subakofn.Serve(server)
}
