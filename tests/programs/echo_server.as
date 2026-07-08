// Echo server: bind an ephemeral port, print it, serve one client:
// uppercase each line until "quit".
var server:ServerSocket = ServerSocket.bind(0);
trace("PORT " + server.localPort);
var client:Socket = server.accept();
var line:String? = client.readLine();
while (line != null && line != "quit") {
    client.write(line.toUpperCase() + "\n");
    line = client.readLine();
}
client.write("bye\n");
client.close();
server.close();
trace("server done");
