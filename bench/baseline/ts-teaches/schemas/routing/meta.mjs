// km は整数なので roundUp の真偽は distance を変えない — 2554 の修復が
// 挿す boolean に stdout が依存しない。
export default {
    mutations: {
        2339: { find: "route.origin", replace: "route.startPoint" },
        2551: { find: "route.origin", replace: "route.origon" },
        2345: { find: 'addStop(route, "harbor", harborKm)', replace: 'addStop(route, harborKm, "harbor")' },
        2554: { find: "totalDistance(route, false)", replace: "totalDistance(route)" },
    },
};
