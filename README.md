
# 项目介绍


## 项目用途

这个代码仓是托管AI-Chatbox项目软件代码的仓库，AI-Chatbox是一个专门用于和LLM对话的盒子硬件。用户可以直接使用语音向LLM提问，盒子获取LLM的回答并播报给用户。目标用户是那些不方便使用手机APP的人群，比如小孩、老人、障人士、还有不想使用手机APP的人士等。


## 开发准备

首先得先准备一个ESP32S3开发板，这个开发板得有语音输入和播报功能。我当前使用XIAO ESP32S3 Sense开发板和一个外接的语音编码硬件来充当软件开发的实验平台。

然后还得有RUST on ESP环境，具体安装过程可以参考[安装RUST on ESP环境](https://paul356.github.io/2024/11/11/rust-on-esp-series_1.html)。有了这两项准备后就开始编译软件了。


## 如何编译

先进入ESPUP环境。

`source $HOME/export-esp.sh`

进入代码目录，执行 `cargo build` 命令。

编译成功后，就可以使用命令 `cargo espflash flash` 将固件上传到ESP32S3开发板上。

然后就可以通过 `cargo espflash monitor` 查看运行日志。

