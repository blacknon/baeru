Output assets/htop-demo.gif

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

# Start normal htop
Type "# 1st step: Run htop only"
Enter
Sleep 500ms
Type "htop"
Enter
Sleep 6s

# exit htop
Type "q"
Sleep 1s


# Start normal htop
Type "# 2nd step: Run htop via baeru"
Enter
Sleep 500ms
Type "baeru -- htop"
Enter
Sleep 6s

# show help
Type "h"
Sleep 1s

# return
Type Escape
Sleep 2s
