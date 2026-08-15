export interface Doc {
    words: string[];
    title: string;
}

export interface Report {
    doc: Doc;
    longest: string;
}

export function scanDoc(title: string, text: string): Doc {
    return { words: text.split(/\s+/).filter((w) => w.length > 0), title };
}

export function buildReport(doc: Doc, minLength: number): Report {
    const eligible = doc.words.filter((w) => w.length >= minLength);
    const longest = eligible.reduce((a, b) => (b.length > a.length ? b : a), "");
    return { doc, longest };
}

export function longWordCount(report: Report, distinct: boolean): number {
    const words = report.doc.words.filter((w) => w.length >= report.longest.length);
    return distinct ? new Set(words).size : words.length;
}
