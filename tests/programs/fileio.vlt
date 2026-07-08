// File IO milestone (SPECS §6, P18): directory lifecycle, metadata,
// copy/rename/append/remove, sorted listing. Self-contained in a
// scratch directory it creates and fully removes.

var dir:String = "vs-fileio-scratch";
if (File.exists(dir)) {
    trace("stale scratch dir — aborting");
    System.exit(1);
}
trace("mkdir " + File.mkdir(dir + "/nested"));
trace("isDirectory " + File.isDirectory(dir) + " " + File.isDirectory(dir + "/nested"));

trace("write " + File.write(dir + "/a.txt", "alpha\n"));
trace("append " + File.append(dir + "/a.txt", "beta\n"));
var body:String? = File.read(dir + "/a.txt");
trace("read " + (body == "alpha\nbeta\n"));
trace("size " + File.size(dir + "/a.txt"));
trace("mtime>0 " + (File.mtime(dir + "/a.txt") > 0));

trace("copy " + File.copy(dir + "/a.txt", dir + "/b.txt"));
trace("rename " + File.rename(dir + "/b.txt", dir + "/nested/c.txt"));

var entries:Array? = File.list(dir);
if (entries != null) {
    trace("list " + entries);
}
var nested:Array? = File.list(dir + "/nested");
if (nested != null) {
    trace("nested " + nested);
}
trace("list-missing " + (File.list(dir + "/nope") == null));

trace("size-missing " + File.size(dir + "/nope.txt"));
trace("remove " + File.remove(dir + "/a.txt"));
trace("remove-c " + File.remove(dir + "/nested/c.txt"));
trace("rmdir-nonempty " + File.rmdir(dir));   // still holds nested/
trace("rmdir-nested " + File.rmdir(dir + "/nested"));
trace("rmdir " + File.rmdir(dir));
trace("gone " + (File.exists(dir) == false));
trace("fileio done");
