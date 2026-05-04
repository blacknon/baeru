Output assets/cli-demo.gif

Require cargo
Require ./target/debug/baeru

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 760
Set Framerate 30
Set Padding 24
Set CursorBlink false
Set TypingSpeed 40ms

# Start normal htop
Type "# 1st step: Run ls only"
Enter
Sleep 500ms
Type "ls -la"
Enter
Sleep 2s

Type "# 2nd step: Run htop via baeru"
Enter
Type "./target/debug/baeru -- ls -la"
Enter
Sleep 4s
