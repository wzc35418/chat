#include "lexer.hpp"
#include "parser.hpp"

#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>

int main(int argc, char* argv[]) {
    if (argc != 3 || (std::string(argv[1]) != "--tokens" &&
                      std::string(argv[1]) != "--ast")) {
        std::cerr << "usage: minic --tokens|--ast source.mc\n";
        return 2;
    }

    try {
        const std::string filename = argv[2];
        std::ifstream input(filename, std::ios::binary);
        if (!input) throw std::runtime_error("cannot open " + filename);
        const std::string source(std::istreambuf_iterator<char>{input}, {});
        const auto tokens = lex(source, filename);

        if (std::string(argv[1]) == "--tokens") {
            for (const Token& token : tokens) {
                std::cout << token.line << ':' << token.column << ' '
                          << token_name(token.kind);
                if (!token.text.empty()) std::cout << " '" << token.text << '\'';
                std::cout << '\n';
            }
        } else {
            Parser parser(tokens, filename);
            const auto expr = parser.parse_return();
            std::cout << "(return " << print_ast(*expr) << ")\n";
        }
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
