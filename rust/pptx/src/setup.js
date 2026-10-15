// Runs after PptxGenJS and before the script: the `pptx` the script fills
// in, and the Subako slide masters, after the subako.ai site.
(function (global) {
  var colors = {
    ink: "16221F",
    strong: "2A3835",
    body: "41504D",
    muted: "606B69",
    paper: "FFFFF6",
    canvas: "EEEEE3",
    sunken: "E1E1D8",
    line: "CDCDCB",
    action: "BFC7C4",
  };
  var fonts = { heading: "Playfair Display", body: "Inter", mono: "IBM Plex Mono" };
  global.subako = { colors: colors, fonts: fonts };

  var pptx = new PptxGenJS();
  pptx.layout = "LAYOUT_16x9"; // 10 x 5.625 in
  pptx.theme = { headFontFace: fonts.heading, bodyFontFace: fonts.body };
  global.pptx = pptx;

  // The logo lockup is 798 x 240.
  var logoInk = "image/png;base64," + __subakoLogoInk;
  var logoPaper = "image/png;base64," + __subakoLogoPaper;
  var footer = { fontFace: fonts.body, fontSize: 9, color: colors.muted };

  // An ink slide with the logo, a large title and a subtitle.
  pptx.defineSlideMaster({
    title: "SUBAKO_TITLE",
    background: { color: colors.ink },
    objects: [
      { image: { x: 0.6, y: 0.5, w: 1.66, h: 0.5, data: logoPaper } },
      { placeholder: { options: { name: "title", type: "title", x: 0.6, y: 1.9, w: 8.8, h: 1.3,
        fontFace: fonts.heading, fontSize: 40, color: colors.paper, valign: "bottom", align: "left" }, text: "" } },
      { placeholder: { options: { name: "subtitle", type: "body", x: 0.6, y: 3.3, w: 8.8, h: 0.9,
        fontFace: fonts.body, fontSize: 16, color: colors.action, valign: "top", align: "left" }, text: "" } },
    ],
  });

  // A paper slide that opens a section: a large title over a rule.
  pptx.defineSlideMaster({
    title: "SUBAKO_SECTION",
    background: { color: colors.canvas },
    objects: [
      { image: { x: 0.6, y: 0.4, w: 1.0, h: 0.3, data: logoInk } },
      { line: { x: 0.6, y: 3.2, w: 8.8, h: 0, line: { color: colors.ink, width: 1 } } },
      { placeholder: { options: { name: "title", type: "title", x: 0.6, y: 1.8, w: 8.8, h: 1.3,
        fontFace: fonts.heading, fontSize: 34, color: colors.ink, valign: "bottom", align: "left" }, text: "" } },
    ],
    slideNumber: Object.assign({ x: 9.0, y: 5.2, w: 0.5, h: 0.3, align: "right" }, footer),
  });

  // The everyday slide: logo and rule up top, a title, a body area for
  // text, tables, charts and images, and a footer.
  pptx.defineSlideMaster({
    title: "SUBAKO_CONTENT",
    background: { color: colors.paper },
    margin: [0.5, 0.5, 0.5, 0.5],
    objects: [
      { image: { x: 0.5, y: 0.3, w: 1.0, h: 0.3, data: logoInk } },
      { line: { x: 0.5, y: 0.75, w: 9.0, h: 0, line: { color: colors.line, width: 0.75 } } },
      { text: { text: "subako.ai", options: Object.assign({ x: 0.5, y: 5.2, w: 3, h: 0.3 }, footer) } },
      { placeholder: { options: { name: "title", type: "title", x: 0.5, y: 0.85, w: 9.0, h: 0.75,
        fontFace: fonts.heading, fontSize: 26, color: colors.ink, valign: "middle", align: "left" }, text: "" } },
      { placeholder: { options: { name: "body", type: "body", x: 0.5, y: 1.7, w: 9.0, h: 3.3,
        fontFace: fonts.body, fontSize: 14, color: colors.body, valign: "top", align: "left" }, text: "" } },
    ],
    slideNumber: Object.assign({ x: 9.0, y: 5.2, w: 0.5, h: 0.3, align: "right" }, footer),
  });
})(globalThis);
