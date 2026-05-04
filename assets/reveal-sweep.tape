Output assets/reveal-sweep.gif

Require ./target/debug/baeru
Require htop

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 760
Set Framerate 30
Set Padding 24
Set CursorBlink false
Set TypingSpeed 0ms

Type "./target/debug/baeru --mode reveal --effect sweep -- htop"
Enter
Sleep 4500ms
Type "q"
Sleep 2s
