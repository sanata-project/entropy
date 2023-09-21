terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 4.0"
    }
  }
}

provider "aws" {
  alias  = "ap-east-1"
  region = "ap-east-1"
}

provider "aws" {
  alias  = "ap-southeast-1"
  region = "ap-southeast-1"
}

provider "aws" {
  alias  = "us-west-1"
  region = "us-west-1"
}

provider "aws" {
  alias  = "eu-central-1"
  region = "eu-central-1"
}

provider "aws" {
  alias  = "sa-east-1"
  region = "sa-east-1"
}

provider "aws" {
  alias  = "af-south-1"
  region = "af-south-1"
}

module "service" {
  source = "./region"
  providers = {
    aws = aws.ap-east-1
  }

  instance_type  = "c5.xlarge"
  instance_count = 1
}

module "region-1" {
  source = "./region"
  providers = {
    aws = aws.ap-east-1
  }

  instance_type = "c5.18xlarge"
  instance_count = 5
}

module "region-2" {
  source = "./region"
  providers = {
    aws = aws.us-west-1
  }

  instance_count = 0
}

module "region-3" {
  source = "./region"
  providers = {
    aws = aws.eu-central-1
  }

  instance_count = 0
}

module "region-4" {
  source = "./region"
  providers = {
    aws = aws.sa-east-1
  }

  instance_count = 0
}

module "region-5" {
  source = "./region"
  providers = {
    aws = aws.af-south-1
  }

  instance_count = 0
}

output "service" {
  value = module.service.instances[0]
}

output "hosts" {
  value = concat(
    module.region-1.instances,
    module.region-2.instances,
    module.region-3.instances,
    module.region-4.instances,
    module.region-5.instances,
  )
}
