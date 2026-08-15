export function scanDoc(title, text) {
    return { words: text.split(/\s+/).filter((w) => w.length > 0), title };
}
export function buildReport(doc, minLength) {
    const eligible = doc.words.filter((w) => w.length >= minLength);
    const longest = eligible.reduce((a, b) => (b.length > a.length ? b : a), "");
    return { doc, longest };
}
export function longWordCount(report, distinct) {
    const words = report.doc.words.filter((w) => w.length >= report.longest.length);
    return distinct ? new Set(words).size : words.length;
}
