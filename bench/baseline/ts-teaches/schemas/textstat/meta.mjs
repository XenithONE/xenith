// 最長語 "compiler"(8字) は一意なので distinct の真偽で count は変わらず、
// 2554 の修復が挿す boolean に stdout が依存しない。
export default {
    mutations: {
        2339: { find: "doc.title", replace: "doc.heading" },
        2551: { find: "doc.title", replace: "doc.tittle" },
        2345: { find: "buildReport(doc, minLength)", replace: "buildReport(minLength, doc)" },
        2554: { find: "longWordCount(report, true)", replace: "longWordCount(report)" },
    },
};
