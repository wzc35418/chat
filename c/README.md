# 小型 C 风格语言：编译器练习

这个目录是独立的编译原理学习项目。编译器使用 C++17 实现，目标是逐步完成一门静态类型、C 风格的语言。

## 当前范围

第一步只处理一条 `return` 语句。表达式支持十进制整数、括号、一元负号、`+`、`-`、`*`、`/`。词法分析记录每个 token 的行号和列号；解析器按常见算术优先级生成语法树。当前尚未生成可执行程序，也没有变量、函数或类型检查。

当前语法（从上到下，优先级逐渐升高）：

```text
program = "return" expression ";" EOF
expression = term { ("+" | "-") term }
term = unary { ("*" | "/") unary }
unary = "-" unary | primary
primary = INTEGER | "(" expression ")"
```

示例输入：

```text
return 1 + 2 * 3;
```

语法树输出：

```text
(return (+ 1 (* 2 3)))
```

## 构建与运行

需要 C++17 编译器和 CMake。在此目录执行；Windows 上先打开 Visual Studio Developer PowerShell 或 Developer Command Prompt：

```sh
cmake -S . -B build
cmake --build build
ctest --test-dir build --output-on-failure
```

运行时传入源码文件和模式：

```sh
./build/minic --tokens examples/precedence.mc
./build/minic --ast examples/precedence.mc
```

Windows 的 Visual Studio 生成器通常把程序放在 `build/Debug/minic.exe`。错误会显示为 `文件:行:列: 信息`。

## 学习路线

1. 阅读 `src/lexer.cpp`，理解字符如何变成 token，并尝试增加 `%`。
2. 阅读 `src/parser.cpp`，理解每层解析函数如何编码运算优先级，并尝试增加比较运算。
3. 加入变量、函数、`if` 和 `while`；随后实现符号表和类型检查。
4. 先输出 C 代码，打通编译执行流程，再引入中间表示和自己的汇编后端。

每增加一项语法，都先写一个合法示例和一个应报错示例。
