const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

const rules = [];
vm.runInNewContext(fs.readFileSync(process.argv[2], "utf8"), {
    polkit: {
        Result: { YES: "authorized" },
        addRule: rule => rules.push(rule),
    },
});
assert.ok(rules.length > 0);

const authorize = (action, groups, active = false) => {
    const subject = { active, local: false, isInGroup: group => groups.includes(group) };
    for (const rule of rules) {
        const result = rule({ id: action }, subject);
        if (result !== undefined) return result;
    }
};
const realtime = "org.freedesktop.RealtimeKit1.acquire-real-time";
const group = process.argv[3];
assert.equal(authorize(realtime, [group]), "authorized");
assert.equal(authorize(realtime, [group], true), "authorized");
assert.equal(authorize(realtime, []), undefined);
assert.equal(authorize(realtime, ["untrusted"]), undefined);
assert.equal(authorize("org.freedesktop.RealtimeKit1.acquire-high-priority", [group]), undefined);
assert.equal(authorize("org.freedesktop.systemd1.manage-units", [group]), undefined);
console.log("RTKit policy: trusted members authorized; other users and actions unchanged");
