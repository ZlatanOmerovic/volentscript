var box:* = 42;
trace(typeof box, box is int, box is String);
box = "text";
trace(typeof box, box is String, box as String);
var loose:* = "5";
trace(loose == 5, loose === 5);
var n:Number = box === "text" ? 1 : 0;
trace(n && 7, 0 || "fallback");
trace(void 0, null);
