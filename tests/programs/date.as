// Date milestone (SPECS §6, ES3 §15.9). Every assertion here is
// timezone-independent: UTC getters on fixed epochs, local-component
// roundtrips, and the UTC/local identity via getTimezoneOffset.

var d:Date = new Date(1751968496789);   // 2025-07-08T09:54:56.789Z
trace(d.getTime());
trace(d.getUTCFullYear() + "-" + (d.getUTCMonth() + 1) + "-" + d.getUTCDate());
trace(d.getUTCDay() + " " + d.getUTCHours() + ":" + d.getUTCMinutes() + ":" + d.getUTCSeconds() + "." + d.getUTCMilliseconds());
trace(d.toUTCString());
trace(d.valueOf() == d.getTime());

var ms:Number = Date.UTC(2026, 6, 8, 12, 30, 45, 250);
var u:Date = new Date(ms);
trace(u.toUTCString());
trace(u.getUTCMilliseconds());

// Local-component constructor reads back the same local components.
var loc:Date = new Date(2026, 0, 15, 10, 20, 30);
trace(loc.getFullYear() + " " + loc.getMonth() + " " + loc.getDate() + " " + loc.getHours() + ":" + loc.getMinutes() + ":" + loc.getSeconds());
trace(loc.getDay());
// §15.9.5.26 identity: local time = UTC - offset minutes.
trace(loc.getTime() == Date.UTC(2026, 0, 15, 10, 20, 30) + loc.getTimezoneOffset() * 60000);

var s:Date = new Date(0);
s.setTime(86400000);
trace(s.toUTCString());

var bad:Date = new Date(0.0 / 0.0);
trace(bad.toString());
trace(bad.getFullYear());

var old:Date = new Date(99, 0, 1);
trace(old.getFullYear());

var a:* = d;
trace(typeof a);
trace(a is Date);
var back:Date? = a as Date;
if (back != null) {
    trace("as " + (back.getTime() == d.getTime()));
}
trace(Date.now() > 1000000000000);
trace("date done");
