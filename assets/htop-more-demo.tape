Output assets/htop-more-demo.gif

Require ./target/debug/baeru
Require htop

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 760
Set Framerate 30
Set Padding 24
Set CursorBlink false
Set TypingSpeed 40ms

# Start baeru htop
Type "# Run htop via baeru"
Enter
Sleep 500ms
Type "./target/debug/baeru -M blacknon -e iTerm -- htop"
Enter
Sleep 20s

# show help
Type "?"
Sleep 2s

# return
Escape
Sleep 6s

# exit
Ctrl+C
Sleep 1s
