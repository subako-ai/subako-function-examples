// Builds a deck from the CSV and image next to this file.
const rows = readText("calls.csv").trim().split("\n").slice(1).map((line) => line.split(","));
const slide = pptx.addSlide({ masterName: "SUBAKO_CONTENT" });
slide.addText("Calls", { placeholder: "title" });
slide.addChart(pptx.ChartType.bar, [{ name: "Calls", labels: rows.map((r) => r[0]), values: rows.map((r) => Number(r[1])) }], { x: 0.5, y: 1.7, w: 6, h: 3 });
slide.addImage({ data: image("paper.png"), x: 7, y: 2, w: 2, h: 0.6 });
console.log(`${rows.length} quarters`);
