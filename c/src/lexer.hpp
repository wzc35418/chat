#pragma once

#include "token.hpp"

#include <string>
#include <vector>

std::vector<Token> lex(const std::string& source, const std::string& filename);
