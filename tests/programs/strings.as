var s:String = "Hello, World";
trace(s.length, s.charAt(1), s.charCodeAt(0));
trace(s.toUpperCase(), s.toLowerCase());
trace(s.indexOf("World"), s.indexOf("x"), s.lastIndexOf("l"));
trace(s.slice(0, 5), s.substring(7), s.substr(7, 5));
trace("a" + 1 + true + null);
var empty:String = "";
trace(empty == "", "b" < "c", "10" == 10);
