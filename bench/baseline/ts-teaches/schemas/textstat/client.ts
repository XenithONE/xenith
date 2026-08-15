import { scanDoc, buildReport, longWordCount } from "./lib";

const minLength = 5;
const doc = scanDoc("field notes", "the compiler taught the model nothing today");
const report = buildReport(doc, minLength);
console.log("title=" + doc.title);
console.log("longest=" + report.longest);
console.log("count=" + longWordCount(report, true));
