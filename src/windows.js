// Loaded into KWin on every capture: reports visible window frames, topmost first,
// as "x,y,w,h,class;..." in global logical coordinates.
const ownPid = __PID__; // quicksnap's own overlays count as windows too
const desk = workspace.currentDesktop;
const act = workspace.currentActivity;
const out = [];
const order = workspace.stackingOrder;
for (let i = order.length - 1; i >= 0; i--) {
    const w = order[i];
    if (w.pid === ownPid || w.minimized || w.desktopWindow || w.dock || w.deleted) continue;
    if (!(w.normalWindow || w.dialog || w.utility)) continue;
    if (!w.onAllDesktops && !w.desktops.some(d => d.id === desk.id)) continue;
    if (w.activities.length && !w.activities.includes(act)) continue;
    const g = w.frameGeometry;
    if (g.width < 10 || g.height < 10) continue;
    out.push([g.x, g.y, g.width, g.height].map(Math.round).join(",") + "," + w.resourceClass);
}
callDBus("dev.triv.QuickSnap", "/dev/triv/QuickSnap", "dev.triv.QuickSnap", "Windows", out.join(";"));
