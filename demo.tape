# Records demo.gif with https://github.com/charmbracelet/vhs
#
#   cargo build --release --bins --examples
#   vhs demo.tape

Output demo.gif

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 640
Set Padding 16
Set Theme "Catppuccin Mocha"
Set TypingSpeed 70ms

Hide
Type "export PATH=$PWD/target/release:$PATH" Enter
Type "./target/release/examples/demo-server 3000 >/dev/null & DEMO=$!" Enter
Type "(sleep 3; while sleep 1.2; do curl -s -o /dev/null localhost:4000/api/users; curl -s -o /dev/null localhost:4000/api/users/2; done) & TRAFFIC=$!" Enter
Type "cd $(mktemp -d) && clear" Enter
Sleep 500ms
Show

Type "apibreak --target http://localhost:3000"
Sleep 500ms
Enter
Sleep 4s

# Break the selected endpoint, watch it fail, then fix it.
Type "5"
Sleep 3s
Type "d"
Sleep 4s
Type "x"
Sleep 2s

# Move to the other endpoint and take it offline.
Type "k"
Sleep 500ms
Type "o"
Sleep 3s
Type "x"
Sleep 1s

Tab
Sleep 500ms
Type "jjjjjj"
Sleep 2s
Tab

Hide
Type "q"
Sleep 300ms
Type "kill $TRAFFIC $DEMO" Enter
