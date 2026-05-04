Output assets/cli-fade.gif

Require ./target/debug/baeru

Set Shell "bash"
Set FontSize 20
Set Width 1120
Set Height 520
Set Framerate 30
Set Padding 24
Set CursorBlink false
Set TypingSpeed 0ms

Type "printf 'service/api        OK    latency=18ms  req/s=421\nservice/worker     WARN  queue=128     lag=3.2s\nservice/db         OK    conn=74       qps=912\n' | env -u NO_COLOR TERM=xterm-256color ./target/debug/baeru --backend cli --effect fade --duration-ms 1800 --frames 42"
Enter
Sleep 3s
